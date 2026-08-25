//! `sinrana-proto`——Sinrana 线上协议的唯一事实来源。
//!
//! 这个 crate 定义了参考服务器和任何客户端
//! 进行协议通信所需的一切：长度前缀分帧、消息信封、
//! 强类型消息集、错误码，以及 [`Transport`] trait。
//!

pub mod error;
pub mod frame;
pub mod message;
pub mod envelope;

mod transport;

#[cfg(feature = "transport-quic")]
pub mod quic;

#[cfg(feature = "transport-quic")]
pub use quic::{QuicTransport, RecvHalf, SendHalf};

pub use error::{ErrorCode, ProtocolError, TransportError};
pub use envelope::Frame;
pub use frame::{FrameCodec, MAX_FRAME};
pub use message::{HistoryItem, Message, Role};
pub use transport::Transport;

/// 当前协议版本。任何破坏性的线上变更都要递增；信封
/// 携带该值，以便对端可以显式拒绝不兼容的版本。
pub const PROTOCOL_VERSION: u16 = 1;
