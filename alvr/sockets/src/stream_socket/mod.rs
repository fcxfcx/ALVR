// Note: for StreamSocket, the client uses a server socket, the server uses a client socket.
// This is because of certificate management. The server needs to trust a client and its certificate
//
// StreamSender and StreamReceiver endpoints allow for convenient conversion of the header to/from
// bytes while still handling the additional byte buffer with zero copies and extra allocations.

mod tcp;
mod udp;

use alvr_common::prelude::*;
use alvr_session::{SocketBufferSize, SocketProtocol};
use bytes::{Buf, BufMut, BytesMut};
use futures::SinkExt;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::HashMap,
    marker::PhantomData,
    mem,
    net::IpAddr,
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tcp::{TcpStreamReceiveSocket, TcpStreamSendSocket};
use tokio::net;
use tokio::sync::{mpsc, Mutex};
use udp::{UdpStreamReceiveSocket, UdpStreamSendSocket};

// 设置套接字缓冲区大小
pub fn set_socket_buffers(
    socket: &socket2::Socket,
    send_buffer_bytes: SocketBufferSize,
    recv_buffer_bytes: SocketBufferSize,
) -> StrResult {
    // 打印初始的套接字缓冲区大小
    info!(
        "Initial socket buffer size: send: {}B, recv: {}B",
        socket.send_buffer_size().map_err(err!())?,
        socket.recv_buffer_size().map_err(err!())?
    );

    {
        // 设置发送套接字缓冲区的大小
        let maybe_size = match send_buffer_bytes {
            SocketBufferSize::Default => None,
            SocketBufferSize::Maximum => Some(u32::MAX),
            SocketBufferSize::Custom(size) => Some(size),
        };

        if let Some(size) = maybe_size {
            if let Err(e) = socket.set_send_buffer_size(size as usize) {
                info!("Error setting socket send buffer: {e}");
            } else {
                info!(
                    "Set socket send buffer succeeded: {}",
                    socket.send_buffer_size().map_err(err!())?
                );
            }
        }
    }

    {
        // 设置接收套接字缓冲区的大小
        let maybe_size = match recv_buffer_bytes {
            SocketBufferSize::Default => None,
            SocketBufferSize::Maximum => Some(u32::MAX),
            SocketBufferSize::Custom(size) => Some(size),
        };

        if let Some(size) = maybe_size {
            if let Err(e) = socket.set_recv_buffer_size(size as usize) {
                info!("Error setting socket recv buffer: {e}");
            } else {
                info!(
                    "Set socket recv buffer succeeded: {}",
                    socket.recv_buffer_size().map_err(err!())?
                );
            }
        }
    }

    Ok(())
}

// 模拟枚举类型，用于表示StreamSender使用的套接字类型
#[derive(Clone)]
enum StreamSendSocket {
    Udp(UdpStreamSendSocket),
    Tcp(TcpStreamSendSocket),
}

// 模拟枚举类型，用于表示StreamReceiver使用的套接字类型
enum StreamReceiveSocket {
    Udp(UdpStreamReceiveSocket),
    Tcp(TcpStreamReceiveSocket),
}

pub struct SendBufferLock<'a> {
    header_bytes: &'a mut BytesMut, // 可变字节缓冲区的头部
    buffer_bytes: BytesMut,         // 不可变字节缓冲区
}

// 实现两个trait，分别是Deref和DerefMut，它们分别用于获取缓冲区的引用和可变引用，转换为BytesMut类型
impl Deref for SendBufferLock<'_> {
    type Target = BytesMut;
    fn deref(&self) -> &BytesMut {
        &self.buffer_bytes // 获取缓冲区的引用
    }
}

impl DerefMut for SendBufferLock<'_> {
    fn deref_mut(&mut self) -> &mut BytesMut {
        &mut self.buffer_bytes // 获取缓冲区的可变引用
    }
}

impl Drop for SendBufferLock<'_> {
    fn drop(&mut self) {
        // the extra split is to avoid moving buffer_bytes
        self.header_bytes.unsplit(self.buffer_bytes.split())
    }
}

// 用于发送数据包，包含一个头部和一个字节缓冲区。它可以将数据包分割成多个碎片，并在每个碎片前加上流ID、包索引、碎片数量和碎片索引等信息。
#[derive(Clone)]
pub struct StreamSender<T> {
    stream_id: u16,
    max_packet_size: usize,
    socket: StreamSendSocket,
    // if the packet index overflows the worst that happens is a false positive packet loss
    next_packet_index: u32,
    _phantom: PhantomData<T>,
}

impl<T: Serialize> StreamSender<T> {
    // 通过feed方法把数据包放到套接字的发送队列中
    async fn send_buffer(&self, buffer: BytesMut) {
        match &self.socket {
            StreamSendSocket::Udp(socket) => socket
                .inner
                .lock()
                .await
                .feed((buffer.freeze(), socket.peer_addr))
                .await
                .map_err(err!())
                .ok(),
            StreamSendSocket::Tcp(socket) => socket
                .lock()
                .await
                .feed(buffer.freeze())
                .await
                .map_err(err!())
                .ok(),
        };
    }

    // 将数据封包并发送，注意这里的header不是网络意义上的包头，而是在业务层进行区分的数据类型
    // 它的作用是方便在接收端进行解包并进行业务处理
    pub async fn send(&mut self, header: &T, buffer: Vec<u8>) -> StrResult {
        // packet layout:
        // [ 2B (stream ID) | 4B (packet index) | 4B (packet shard count) | 4B (shard index)]
        // this escluses length delimited coding, which is handled by the TCP backend
        const OFFSET: usize = 2 + 4 + 4 + 4;
        // 计算每个碎片（payload）的最大大小
        let max_payload_size = self.max_packet_size - OFFSET;

        let data_header_size = bincode::serialized_size(header).map_err(err!()).unwrap() as usize;

        // 对数据缓冲区进行分片，每个碎片的大小为max_payload_size
        let shards = buffer.chunks(max_payload_size);
        let shards_count = shards.len() + 1;

        // 为每个碎片添加一个包头，包含以下信息：流ID、包索引、碎片总数和碎片索引
        // 同时在send_buffer方法里调用相应的feed方法将每个碎片放入发送缓冲区
        let mut shards_buffer =
            BytesMut::with_capacity(data_header_size + buffer.len() + shards_count * OFFSET);

        // 首先把header的数据发过去，注意这里header是序列化发过去的，方便之后的解包反序列化
        {
            shards_buffer.put_u16(self.stream_id);
            shards_buffer.put_u32(self.next_packet_index);
            shards_buffer.put_u32(shards_count as _);
            shards_buffer.put_u32(0);
            shards_buffer.put_slice(&bincode::serialize(header).map_err(err!()).unwrap());
            self.send_buffer(shards_buffer.split()).await;
        }

        // 然后把剩下buffer部分的数据发过去，会为每个分片加上包头
        for (shard_index, shard) in shards.enumerate() {
            shards_buffer.put_u16(self.stream_id);
            shards_buffer.put_u32(self.next_packet_index);
            shards_buffer.put_u32(shards_count as _);
            shards_buffer.put_u32((shard_index + 1) as _);
            shards_buffer.put_slice(shard);
            self.send_buffer(shards_buffer.split()).await;
        }

        // 根据套接字类型（socket）调用flush方法将缓冲区中的数据发送出去，先全部feed完再flush，目的是减少系统调用
        match &self.socket {
            StreamSendSocket::Udp(socket) => {
                socket.inner.lock().await.flush().await.map_err(err!())?;
            }
            StreamSendSocket::Tcp(socket) => socket.lock().await.flush().await.map_err(err!())?,
        }

        //  将包索引加1
        self.next_packet_index += 1;

        Ok(())
    }
}

// 用于存储和管理接收的数据包
pub struct ReceiverBuffer<T> {
    inner: BytesMut,          // 可变字节缓冲区，用于存储接收到的数据
    had_packet_loss: bool,    // 是否发生了包丢失
    _phantom: PhantomData<T>, // 用于标记类型T
}

impl<T> ReceiverBuffer<T> {
    pub fn new() -> Self {
        Self {
            inner: BytesMut::new(),
            had_packet_loss: false, // 默认没有包丢失
            _phantom: PhantomData,
        }
    }

    pub fn had_packet_loss(&self) -> bool {
        self.had_packet_loss
    }
}

impl<T: DeserializeOwned> ReceiverBuffer<T> {
    // 用于从接收缓冲区中获取数据包，返回一个元组，包含数据包头和数据包的剩余部分（切分）
    // DeserializeOwned表示T必须是可反序列化的
    pub fn get(&self) -> StrResult<(T, &[u8])> {
        let mut data: &[u8] = &self.inner;
        let header = bincode::deserialize_from(&mut data).map_err(err!())?;

        Ok((header, data))
    }
}

// 用于接收数据包，包含一个通道接收器和一个哈希表。它可以从多个碎片中重构数据包，并检查是否有丢失的碎片。
pub struct StreamReceiver<T> {
    receiver: mpsc::UnboundedReceiver<BytesMut>, // 一个无界的消息传递通道（mpsc）接收器，用于接收来自套接字的数据。
    next_packet_shards: HashMap<usize, BytesMut>, //一个哈希表，用于存储下一个数据包的分片
    next_packet_shards_count: Option<usize>,     // 下一个数据包的分片总数
    next_packet_index: u32,                      // 下一个数据包的索引
    _phantom: PhantomData<T>,                    // 用于标记类型T
}

/// Get next packet reconstructing from shards. It can store at max shards from two packets; if the
/// reordering entropy is too high, packets will never be successfully reconstructed.
impl<T: DeserializeOwned> StreamReceiver<T> {
    // 这个方法最多保存两个包的数据分片
    pub async fn recv_buffer(&mut self, buffer: &mut ReceiverBuffer<T>) -> StrResult {
        buffer.had_packet_loss = false;

        // 接收包的循环
        loop {
            // 确认目前的包索引是多少，然后将包索引加1
            let current_packet_index = self.next_packet_index;
            self.next_packet_index += 1;

            // 交换当前哈希表和下一个哈希表中存储的分片（因为上一次循环里可能保存了当前处理的数据的分片）
            let mut current_packet_shards =
                HashMap::with_capacity(self.next_packet_shards.capacity());
            mem::swap(&mut current_packet_shards, &mut self.next_packet_shards);

            let mut current_packet_shards_count = self.next_packet_shards_count.take();

            // 循环直到获取了一个包的所有分片
            loop {
                if let Some(shards_count) = current_packet_shards_count {
                    if current_packet_shards.len() >= shards_count {
                        buffer.inner.clear();

                        for i in 0..shards_count {
                            // 组装分片
                            if let Some(shard) = current_packet_shards.get(&i) {
                                buffer.inner.put_slice(shard);
                            } else {
                                // 缺少某一个index的分片，说明发生了包丢失
                                error!("Cannot find shard with given index!");
                                buffer.had_packet_loss = true;

                                self.next_packet_shards.clear();

                                break;
                            }
                        }

                        return Ok(());
                    }
                }

                // 利用通道接收器接收数据包分片
                let mut shard = self.receiver.recv().await.ok_or_else(enone!())?;

                // 从分片头中获取分片索引、分片总数和分片索引
                let shard_packet_index = shard.get_u32();
                let shards_count = shard.get_u32() as usize;
                let shard_index = shard.get_u32() as usize;

                if shard_packet_index == current_packet_index {
                    // 如果分片头的包索引等于当前包索引，则将分片存储到当前哈希表中
                    current_packet_shards.insert(shard_index, shard);
                    current_packet_shards_count = Some(shards_count);
                } else if shard_packet_index >= self.next_packet_index {
                    // 判断如果收到了非当前包的数据
                    if shard_packet_index > self.next_packet_index {
                        // 如果收到的是非下一个包的分片，则先清空存储下一个包数据的哈希表（代表有新的包发过来了）
                        self.next_packet_shards.clear();
                    }

                    self.next_packet_shards.insert(shard_index, shard); //  将分片存储到存放下一个包数据的哈希表中
                    self.next_packet_shards_count = Some(shards_count); // 获取下一个包的分片总数
                    self.next_packet_index = shard_packet_index; // 获取下一个包的索引

                    if shard_packet_index > self.next_packet_index
                        || self.next_packet_shards.len() == shards_count
                    {
                        // 如果收到的是非下一个包的分片（比如下下个），或者收到了下一个包的所有分片，则跳出循环，开始处理下一个包
                        // 当前包会被认为是丢失的
                        debug!("Skipping to next packet. Signaling packet loss.");
                        buffer.had_packet_loss = true;
                        break;
                    }
                }
                // else: ignore old shard
            }
        }
    }

    pub async fn recv_header_only(&mut self) -> StrResult<T> {
        // 只接收头部
        // 注意，这里的包是业务层面的“包”，头也是业务层面的“头”，并不是网络层面的
        let mut buffer = ReceiverBuffer::new();
        self.recv_buffer(&mut buffer).await?;

        Ok(buffer.get()?.0)
    }
}

pub enum StreamSocketBuilder {
    Tcp(net::TcpListener),
    Udp(net::UdpSocket),
}

impl StreamSocketBuilder {
    pub async fn listen_for_server(
        port: u16,
        stream_socket_config: SocketProtocol,
        send_buffer_bytes: SocketBufferSize,
        recv_buffer_bytes: SocketBufferSize,
    ) -> StrResult<Self> {
        // 这个方法是给client端使用的，用于监听端口
        Ok(match stream_socket_config {
            SocketProtocol::Udp => StreamSocketBuilder::Udp(
                udp::bind(port, send_buffer_bytes, recv_buffer_bytes).await?,
            ),
            SocketProtocol::Tcp => StreamSocketBuilder::Tcp(
                tcp::bind(port, send_buffer_bytes, recv_buffer_bytes).await?,
            ),
        })
    }

    pub async fn accept_from_server(
        self,
        server_ip: IpAddr,
        port: u16,
        max_packet_size: usize,
    ) -> StrResult<StreamSocket> {
        // 用于接收连接，返回一个StreamSocket
        // 这个方法同样是给client端使用的
        let (send_socket, receive_socket) = match self {
            StreamSocketBuilder::Udp(socket) => {
                let (send_socket, receive_socket) = udp::connect(socket, server_ip, port).await?;
                (
                    StreamSendSocket::Udp(send_socket),
                    StreamReceiveSocket::Udp(receive_socket),
                )
            }
            StreamSocketBuilder::Tcp(listener) => {
                let (send_socket, receive_socket) =
                    tcp::accept_from_server(listener, server_ip).await?;
                (
                    StreamSendSocket::Tcp(send_socket),
                    StreamReceiveSocket::Tcp(receive_socket),
                )
            }
        };

        Ok(StreamSocket {
            max_packet_size,
            send_socket,
            receive_socket: Arc::new(Mutex::new(Some(receive_socket))),
            packet_queues: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn connect_to_client(
        client_ip: IpAddr,
        port: u16,
        protocol: SocketProtocol,
        send_buffer_bytes: SocketBufferSize,
        recv_buffer_bytes: SocketBufferSize,
        max_packet_size: usize,
    ) -> StrResult<StreamSocket> {
        // 这个方法是由server端使用的，用于连接client端
        let (send_socket, receive_socket) = match protocol {
            SocketProtocol::Udp => {
                let socket = udp::bind(port, send_buffer_bytes, recv_buffer_bytes).await?;
                let (send_socket, receive_socket) = udp::connect(socket, client_ip, port).await?;
                (
                    StreamSendSocket::Udp(send_socket),
                    StreamReceiveSocket::Udp(receive_socket),
                )
            }
            SocketProtocol::Tcp => {
                let (send_socket, receive_socket) =
                    tcp::connect_to_client(client_ip, port, send_buffer_bytes, recv_buffer_bytes)
                        .await?;
                (
                    StreamSendSocket::Tcp(send_socket),
                    StreamReceiveSocket::Tcp(receive_socket),
                )
            }
        };

        Ok(StreamSocket {
            max_packet_size,
            send_socket,
            receive_socket: Arc::new(Mutex::new(Some(receive_socket))),
            packet_queues: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

pub struct StreamSocket {
    max_packet_size: usize,                                  // 最大包大小
    send_socket: StreamSendSocket,                           // 发送socket
    receive_socket: Arc<Mutex<Option<StreamReceiveSocket>>>, // 接收socket
    packet_queues: Arc<Mutex<HashMap<u16, mpsc::UnboundedSender<BytesMut>>>>, // 存放接收到的包的队列
}

impl StreamSocket {
    pub async fn request_stream<T>(&self, stream_id: u16) -> StrResult<StreamSender<T>> {
        // 构建一个StreamSender对象
        Ok(StreamSender {
            stream_id,
            max_packet_size: self.max_packet_size,
            socket: self.send_socket.clone(),
            next_packet_index: 0, //把包索引先设为0
            _phantom: PhantomData,
        })
    }

    pub async fn subscribe_to_stream<T>(&self, stream_id: u16) -> StrResult<StreamReceiver<T>> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.packet_queues.lock().await.insert(stream_id, sender); // 把流id和负责这个流的sender储存到哈希表里

        // 返回一个StreamReceiver对象，把receiver传给它，这个receiver对应的是无界消息传递通道的接收端
        Ok(StreamReceiver {
            receiver,
            next_packet_shards: HashMap::new(),
            next_packet_shards_count: None,
            next_packet_index: 0,
            _phantom: PhantomData,
        })
    }

    pub async fn receive_loop(&self) -> StrResult {
        // 去调用两种协议的接收循环，详情去看udp.rs和tcp.rs
        match self.receive_socket.lock().await.take().unwrap() {
            StreamReceiveSocket::Udp(socket) => {
                udp::receive_loop(socket, Arc::clone(&self.packet_queues)).await
            }
            StreamReceiveSocket::Tcp(socket) => {
                tcp::receive_loop(socket, Arc::clone(&self.packet_queues)).await
            }
        }
    }
}
