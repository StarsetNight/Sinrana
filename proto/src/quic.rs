//! 单个双向流上基于 QUIC 的 [`Transport`]。
//!
//! 这在 `quinn` 双向流之上实现协议的传输契约。
//! 它是服务器和 CLI 客户端共同使用的参考传输。
//! 所有分帧（长度前缀 + msgpack）都在此流上处理；
//! 端点配置和证书处理
//! 位于服务器/客户端 crate 中。
//!
//! 这个模块仅在 `transport-quic` 特性下编译，以便
//! 协议核心保持轻依赖。

use crate::{frame::FrameCodec, Frame, Transport, TransportError};
use quinn::{RecvStream, SendStream};
use tokio::io::AsyncWriteExt;

/// QUIC 双向流上的具体 [`Transport`]。
///
/// 每个连接都会创建一个这样的实例，由单个双向流支撑。会话的所有
/// 帧都在该流上流动，这提供了每个方向上的可靠、有序
/// 投递，并让协议可以忽略消息边界
///（没有 `\0` 分隔符，没有粘包 hack）。
pub struct QuicTransport {
    recv: RecvStream,
    send: SendStream,
    codec: FrameCodec,
}

/// [`QuicTransport`] 的发送半边：拥有出站流。
pub struct SendHalf {
    send: SendStream,
}

/// [`QuicTransport`] 的接收半边：拥有入站流和编解码器。
pub struct RecvHalf {
    recv: RecvStream,
    codec: FrameCodec,
}

impl QuicTransport {
    /// 包裹一个已拆分的 QUIC 双向流对。
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            recv,
            send,
            codec: FrameCodec::new(),
        }
    }

    /// 拆分为独立的发送和接收半边，这样连接可以由
    /// 一个写入任务和一个读取任务并发驱动。
    pub fn split(self) -> (SendHalf, RecvHalf) {
        (
            SendHalf { send: self.send },
            RecvHalf {
                recv: self.recv,
                codec: self.codec,
            },
        )
    }

    /// 消费该传输并返回底层流。
    pub fn into_parts(self) -> (SendStream, RecvStream) {
        (self.send, self.recv)
    }
}

impl SendHalf {
    /// 向流写入一个长度前缀、msgpack 编码的帧。
    pub async fn send(&mut self, frame: &Frame) -> Result<(), TransportError> {
        let wire = frame.to_wire()?;
        self.send
            .write_all(&wire)
            .await
            .map_err(|e| TransportError::other(e.to_string()))?;
        self.send
            .flush()
            .await
            .map_err(|e| TransportError::other(e.to_string()))?;
        Ok(())
    }
}

impl RecvHalf {
    /// 读取下一个入站帧，当对端关闭时返回 `Closed`。
    pub async fn recv(&mut self) -> Result<Frame, TransportError> {
        let mut buf = [0u8; 8192];
        loop {
            if let Some(payload) = self.codec.try_pop()? {
                return Frame::from_payload_v(&payload);
            }
            match self.recv.read(&mut buf).await.map_err(|e| TransportError::other(e.to_string()))? {
                Some(0) => return Err(TransportError::Closed),
                Some(n) => self.codec.push(&buf[..n])?,
                None => return Err(TransportError::Closed),
            }
        }
    }
}

impl Transport for QuicTransport {
    fn send(&mut self, frame: &Frame) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
        async move {
            let wire = frame.to_wire()?;
            self.send
                .write_all(&wire)
                .await
                .map_err(|e| TransportError::other(e.to_string()))?;
            self.send
                .flush()
                .await
                .map_err(|e| TransportError::other(e.to_string()))?;
            Ok(())
        }
    }

    fn recv(&mut self) -> impl std::future::Future<Output = Result<Frame, TransportError>> + Send {
        async move {
            let mut buf = [0u8; 8192];
            loop {
                if let Some(payload) = self.codec.try_pop()? {
                    return Frame::from_payload_v(&payload);
                }
                match self.recv.read(&mut buf).await.map_err(|e| TransportError::other(e.to_string()))? {
                    Some(0) => return Err(TransportError::Closed),
                    Some(n) => self.codec.push(&buf[..n])?,
                    None => return Err(TransportError::Closed),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // QUIC 传输本身需要活动连接，因此它在 `server`
    // crate 的端到端测试中得到验证。
    fn _nothing() {}
}
