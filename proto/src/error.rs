//! Sinrana 协议的带类型错误模型。
//!
//! `ProtocolError` 在 `sys.error` 命名空间中的 [`crate::Message::Error`]
//! 帧内传输。它携带一个稳定的机器可读 [`ErrorCode`]，
//! 以及一条人类可读的消息和一个 `retryable` 提示，因此客户端既
//! 可以显示合理的内容，也可以决定是否重试。

use serde::{Deserialize, Serialize};

/// 对端可以据此分支的稳定机器可读错误码。
///
/// 代码值一旦发布就在各协议版本间保持稳定；不要
/// 重新编号现有的变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// 请求格式错误或缺少必填字段。
    BadRequest,
    /// 认证失败（错误的用户名/密码/令牌）。
    AuthFailed,
    /// 发送者未认证 / 会话无效或已过期。
    NotAuthenticated,
    /// 发送者缺少此操作所需的角色/权限。
    Forbidden,
    /// 请求的资源（用户、房间、消息）不存在。
    NotFound,
    /// 资源已存在（重复的用户名、房间名，……）。
    Conflict,
    /// 由于速率限制或背压，操作被拒绝。
    RateLimited,
    /// 意外的服务端故障。
    Internal,
    /// 对端使用此端点无法理解的协议版本。
    VersionMismatch,
    /// 协议级别的校验失败（错误的分帧、错误的信封，……）。
    Protocol,
    /// 目标房间不存在。
    RoomNotFound,
    /// 目标用户已存在（在注册期间）。
    UserExists,
    /// 调用者不是所请求房间的成员。
    NotInRoom,
    /// 仅由服务器为特殊名称保留。
    ReservedName,
}

impl ErrorCode {
    /// 每个代码的简短默认人类可读消息。服务器可以
    /// 覆盖它，但这给客户端一个合理的回退。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "Malformed request",
            Self::AuthFailed => "Authentication failed",
            Self::NotAuthenticated => "Not authenticated",
            Self::Forbidden => "Permission denied",
            Self::NotFound => "Not found",
            Self::Conflict => "Already exists",
            Self::RateLimited => "Rate limited",
            Self::Internal => "Internal server error",
            Self::VersionMismatch => "Protocol version mismatch",
            Self::Protocol => "Protocol error",
            Self::RoomNotFound => "Room not found",
            Self::UserExists => "User already exists",
            Self::NotInRoom => "Not in room",
            Self::ReservedName => "Reserved name",
        }
    }
}

/// 在 `sys.error` 帧中携带的结构化错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl ProtocolError {
    /// 从代码构造，使用默认消息文本。
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            message: code.as_str().to_string(),
            retryable: false,
        }
    }

    /// 覆盖默认消息文本（构建器风格）。
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    /// 从代码和显式消息构造。
    pub fn with_text(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    /// 将错误标记为可重试（例如临时速率限制）。
    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

/// 在发送/接收帧时由传输层暴露的错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// 底层的 I/O 错误（套接字、流读写，……）。
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// 帧编码为 msgpack 失败。
    #[error("encode: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    /// 帧载荷从 msgpack 解码失败。
    #[error("decode: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    /// 对端关闭了流 / 连接。
    #[error("connection closed")]
    Closed,
    /// 任何其他传输故障。
    #[error("transport: {0}")]
    Other(String),
}

impl TransportError {
    /// 从字符串创建一个通用的 `Other` 错误。
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_round_trips_through_msgpack() {
        for code in [
            ErrorCode::BadRequest,
            ErrorCode::AuthFailed,
            ErrorCode::VersionMismatch,
            ErrorCode::ReservedName,
        ] {
            let bytes = rmp_serde::to_vec_named(&code).unwrap();
            let decoded: ErrorCode = rmp_serde::from_slice(&bytes).unwrap();
            assert_eq!(code, decoded);
        }
    }

    #[test]
    fn protocol_error_round_trips() {
        let err = ProtocolError::with_text(ErrorCode::RoomNotFound, "no such room").retryable();
        let bytes = rmp_serde::to_vec_named(&err).unwrap();
        let decoded: ProtocolError = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(err, decoded);
        assert!(decoded.retryable);
    }
}
