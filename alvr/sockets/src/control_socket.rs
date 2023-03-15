use super::{Ldc, CONTROL_PORT, LOCAL_IP};
use alvr_common::prelude::*;
use bytes::Bytes;
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{marker::PhantomData, net::IpAddr};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

// 控制信息的socket和数据流socket相似，不过只用TCP
pub struct ControlSocketSender<T> {
    inner: SplitSink<Framed<TcpStream, Ldc>, Bytes>,
    _phantom: PhantomData<T>,
}

impl<S: Serialize> ControlSocketSender<S> {
    pub async fn send(&mut self, packet: &S) -> StrResult {
        // 发送是通过序列化的方式发送数据的
        let packet_bytes = bincode::serialize(packet).map_err(err!())?;
        self.inner.send(packet_bytes.into()).await.map_err(err!())
    }
}

pub struct ControlSocketReceiver<T> {
    inner: SplitStream<Framed<TcpStream, Ldc>>,
    _phantom: PhantomData<T>,
}

impl<R: DeserializeOwned> ControlSocketReceiver<R> {
    pub async fn recv(&mut self) -> StrResult<R> {
        // 接收数据并反序列化
        let packet_bytes = self
            .inner
            .next()
            .await
            .ok_or_else(enone!())?
            .map_err(err!())?;
        bincode::deserialize(&packet_bytes).map_err(err!())
    }
}

pub async fn get_server_listener() -> StrResult<TcpListener> {
    TcpListener::bind((LOCAL_IP, CONTROL_PORT))
        .await
        .map_err(err!())
}

// Proto-control-socket that can send and receive any packet. After the split, only the packets of
// the specified types can be exchanged
pub struct ProtoControlSocket {
    inner: Framed<TcpStream, Ldc>,
}

pub enum PeerType<'a> {
    AnyClient(Vec<IpAddr>),
    Server(&'a TcpListener),
}

impl ProtoControlSocket {
    // 注意，这里的客户端仅指的是发起连接的一方，并不是ALVR的client，因为在ALVR里，client是接受server发起的连接的一方
    pub async fn connect_to(peer: PeerType<'_>) -> StrResult<(Self, IpAddr)> {
        let socket = match peer {
            // 作为发起连接的一方
            PeerType::AnyClient(ips) => {
                let client_addresses = ips
                    .iter()
                    .map(|&ip| (ip, CONTROL_PORT).into())
                    .collect::<Vec<_>>();
                TcpStream::connect(client_addresses.as_slice())
                    .await
                    .map_err(err!())?
            }
            // 作为接受连接的一方
            PeerType::Server(listener) => {
                let (socket, _) = listener.accept().await.map_err(err!())?;
                socket
            }
        };

        socket.set_nodelay(true).map_err(err!())?;
        let peer_ip = socket.peer_addr().map_err(err!())?.ip();
        // 还是一样的利用Framed让socket可以发送和接收固定长度的数据
        let socket = Framed::new(socket, Ldc::new());

        Ok((Self { inner: socket }, peer_ip))
    }

    pub async fn send<S: Serialize>(&mut self, packet: &S) -> StrResult {
        // 序列化，发送数据
        let packet_bytes = bincode::serialize(packet).map_err(err!())?;
        self.inner.send(packet_bytes.into()).await.map_err(err!())
    }

    pub async fn recv<R: DeserializeOwned>(&mut self) -> StrResult<R> {
        // 用next方法接收数据，然后反序列化
        let packet_bytes = self
            .inner
            .next()
            .await
            .ok_or_else(enone!())?
            .map_err(err!())?;
        bincode::deserialize(&packet_bytes).map_err(err!())
    }

    pub fn split<S: Serialize, R: DeserializeOwned>(
        self,
    ) -> (ControlSocketSender<S>, ControlSocketReceiver<R>) {
        // 分割为发送和接收两个socket，Sink是发送的，Stream是接收的
        let (sender, receiver) = self.inner.split();

        (
            ControlSocketSender {
                inner: sender,
                _phantom: PhantomData,
            },
            ControlSocketReceiver {
                inner: receiver,
                _phantom: PhantomData,
            },
        )
    }
}
