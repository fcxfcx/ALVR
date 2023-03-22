use crate::{Ldc, LOCAL_IP};
use alvr_common::prelude::*;
use alvr_session::SocketBufferSize;
use bytes::{Buf, Bytes, BytesMut};
use futures::{
    stream::{SplitSink, SplitStream},
    StreamExt,
};
use std::{collections::HashMap, net::IpAddr, sync::Arc};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, Mutex},
};
use tokio_util::codec::Framed;

pub type TcpStreamSendSocket = Arc<Mutex<SplitSink<Framed<TcpStream, Ldc>, Bytes>>>;
pub type TcpStreamReceiveSocket = SplitStream<Framed<TcpStream, Ldc>>;

pub async fn bind(
    port: u16,
    send_buffer_bytes: SocketBufferSize,
    recv_buffer_bytes: SocketBufferSize,
) -> StrResult<TcpListener> {
    // 构造一个socket
    let socket = TcpListener::bind((LOCAL_IP, port)).await.map_err(err!())?;
    let socket = socket2::Socket::from(socket.into_std().map_err(err!())?);

    // 设置socket的发送缓冲区和接收缓冲区大小
    super::set_socket_buffers(&socket, send_buffer_bytes, recv_buffer_bytes).ok();

    // 转化为TcpListener返回
    TcpListener::from_std(socket.into()).map_err(err!())
}

pub async fn accept_from_server(
    listener: TcpListener,
    server_ip: IpAddr,
) -> StrResult<(TcpStreamSendSocket, TcpStreamReceiveSocket)> {
    // 接收服务器传来的连接
    let (socket, server_address) = listener.accept().await.map_err(err!())?;

    // 判断是否连接到了正确的服务器ip
    if server_address.ip() != server_ip {
        return fmt_e!("Connected to wrong client: {server_address} != {server_ip}");
    }

    socket.set_nodelay(true).map_err(err!())?;
    // 利用frame，让socket可以发送和接收固定长度的数据
    let socket = Framed::new(socket, Ldc::new());
    let (send_socket, receive_socket) = socket.split();

    // 返回发送和接收的socket
    Ok((Arc::new(Mutex::new(send_socket)), receive_socket))
}

pub async fn connect_to_client(
    client_ip: IpAddr,
    port: u16,
    send_buffer_bytes: SocketBufferSize,
    recv_buffer_bytes: SocketBufferSize,
) -> StrResult<(TcpStreamSendSocket, TcpStreamReceiveSocket)> {
    // 服务器向客户端发起连接
    let socket = TcpStream::connect((client_ip, port))
        .await
        .map_err(err!())?;
    let socket = socket2::Socket::from(socket.into_std().map_err(err!())?);

    // 转化到socket对象配置一下缓冲区大小
    super::set_socket_buffers(&socket, send_buffer_bytes, recv_buffer_bytes).ok();

    let socket = TcpStream::from_std(socket.into()).map_err(err!())?;
    socket.set_nodelay(true).map_err(err!())?;
    // 剩下的部分和client的一样
    let socket = Framed::new(socket, Ldc::new());
    let (send_socket, receive_socket) = socket.split();

    Ok((Arc::new(Mutex::new(send_socket)), receive_socket))
}

pub async fn receive_loop(
    mut socket: TcpStreamReceiveSocket,
    packet_enqueuers: Arc<Mutex<HashMap<u16, mpsc::UnboundedSender<BytesMut>>>>,
) -> StrResult {
    while let Some(maybe_packet) = socket.next().await {
        let mut packet = maybe_packet.map_err(err!())?;

        // socket收到一个包后解析流索引，然后根据id找到对应的UnboundedSender，通过它发送包数据（这个数据会被StreamReceiver处理）
        let stream_id = packet.get_u16();
        if let Some(enqueuer) = packet_enqueuers.lock().await.get_mut(&stream_id) {
            enqueuer.send(packet).map_err(err!())?;
        }
    }

    Ok(())
}
