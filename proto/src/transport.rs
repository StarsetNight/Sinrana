//! 传输抽象。
//!
//! [`Transport`] 将协议与具体的字节管道解耦。核心
//! 帧格式和消息集与传输无关；一个传输只需要
//! 可靠地按顺序移动长度前缀帧。参考实现
//! 使用 QUIC（特性 `transport-quic`）；以后可以针对同一个 trait
//! 添加一个普通的 TCP 传输。

use crate::Frame;

/// 一个双向、有序、面向消息的连接，用于移动 [`Frame`]。
///
/// 为了协议正确，一个传输必须保证的语义：
/// * 帧在每个方向上**只投递一次，并按发送顺序**；
/// * 两个方向相互独立；
/// * 当对端干净地关闭时，报告 [`TransportError::Closed`]。
pub trait Transport {
    /// 发送一个帧，在传输需要时刷新。
    fn send(
        &mut self,
        frame: &Frame,
    ) -> impl std::future::Future<Output = Result<(), crate::TransportError>> + Send;

    /// 接收下一个帧，当对端关闭时返回 `Closed`。
    fn recv(&mut self) -> impl std::future::Future<Output = Result<Frame, crate::TransportError>> + Send;
}
