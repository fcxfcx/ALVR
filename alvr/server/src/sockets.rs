use alvr_common::{anyhow::Result, con_bail, ConResult, HandleTryAgain, ToCon, ALVR_NAME};
use alvr_sockets::{CONTROL_PORT, HANDSHAKE_PACKET_SIZE_BYTES, LOCAL_IP};
use std::{
    net::{IpAddr, UdpSocket},
    time::Duration,
};

pub struct WelcomeSocket {
    // 用的是标准库std里面的UdpSocket
    socket: UdpSocket,
    buffer: [u8; HANDSHAKE_PACKET_SIZE_BYTES],
}

impl WelcomeSocket {
    pub fn new(read_timeout: Duration) -> Result<Self> {
        // 绑定本地IP和端口9943（Control Port）
        let socket = UdpSocket::bind((LOCAL_IP, CONTROL_PORT))?;
        // 设置为非阻塞，如果没有数据可读或可写会直接返回一个错误
        socket.set_read_timeout(Some(read_timeout))?;
        // buffer目前大小是56字节
        Ok(Self {
            socket,
            buffer: [0; HANDSHAKE_PACKET_SIZE_BYTES],
        })
    }

    // Returns: client IP, client hostname
    pub fn recv(&mut self) -> ConResult<(String, IpAddr)> {
        let (size, address) = self.socket.recv_from(&mut self.buffer).handle_try_again()?;

        // 判断收到的数据包是否符合格式（56字节，前4字节是ALVR，后到16字节是0）
        // 一个完整的数据包格式是：| 4位 ALVR | 12位 0 | 8位 protocol_id | 32位 hostname |
        if size == HANDSHAKE_PACKET_SIZE_BYTES
            && &self.buffer[..ALVR_NAME.len()] == ALVR_NAME.as_bytes()
            && self.buffer[ALVR_NAME.len()..16].iter().all(|b| *b == 0)
        {
            // 解析16-24的8个字节为protocol_id
            let mut protocol_id_bytes = [0; 8];
            protocol_id_bytes.copy_from_slice(&self.buffer[16..24]);
            // 把字节还原为数字
            let received_protocol_id = u64::from_le_bytes(protocol_id_bytes);

            // 对比收到的protocol_id和当前使用的protocol_id是否一致（这个id是由ALVR版本hash处理得来的）
            if received_protocol_id != alvr_common::protocol_id() {
                con_bail!("Found incompatible client! Upgrade or downgrade\nExpected protocol ID {}, Found {received_protocol_id}",
                alvr_common::protocol_id());
            }

            // 解析24到56的32个字节为hostname
            let mut hostname_bytes = [0; 32];
            hostname_bytes.copy_from_slice(&self.buffer[24..56]);
            let hostname = std::str::from_utf8(&hostname_bytes)
                .to_con()?
                .trim_end_matches('\x00')
                .to_owned();

            // 解析完毕后返回客户端的IP和hostname
            Ok((hostname, address.ip()))
        } else if &self.buffer[..16] == b"\x00\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00ALVR"
            || &self.buffer[..5] == b"\x01ALVR"
        {
            con_bail!("Found old client. Please upgrade")
        } else {
            // Unexpected packet.
            // Note: no need to check for v12 and v13, not found in the wild anymore
            con_bail!("Found unrelated packet during discovery")
        }
    }
}
