//! 在长度前缀帧内部携带的协议信封。
//!
//! [`Frame`] 用协议版本和一个关联
//! 标识符来包裹 [`Message`]。请求帧使用非零 `id`；匹配的响应和任何错误
//! 都复用该 `id`。服务端推送使用 `id == 0`。

use crate::{frame, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};

/// 每个帧都携带的信封。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// 协议版本。对端必须显式拒绝不匹配的版本。
    pub ver: u16,
    /// 关联标识符。`0` 表示“无关联”（服务端推送）。
    pub id: u64,
    /// 消息本身。
    pub msg: crate::Message,
}

impl Frame {
    /// 用给定的关联标识符构造一个请求帧。
    pub fn request(id: u64, msg: crate::Message) -> Self {
        Self {
            ver: PROTOCOL_VERSION,
            id,
            msg,
        }
    }

    /// 构造一个回显请求 `id` 的响应帧。
    pub fn response(id: u64, msg: crate::Message) -> Self {
        Self {
            ver: PROTOCOL_VERSION,
            id,
            msg,
        }
    }

    /// 构造一个服务端发起的推送帧（`id == 0`）。
    pub fn push(msg: crate::Message) -> Self {
        Self {
            ver: PROTOCOL_VERSION,
            id: 0,
            msg,
        }
    }

    /// 一个回显 `id` 的便捷错误响应。
    pub fn error(id: u64, err: crate::ProtocolError) -> Self {
        Self {
            ver: PROTOCOL_VERSION,
            id,
            msg: crate::Message::Error {
                code: err.code,
                message: err.message,
                retryable: err.retryable,
            },
        }
    }

    /// 编码为完整的线上形式（长度前缀 + msgpack 载荷）。
    pub fn to_wire(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        let payload = rmp_serde::to_vec_named(self)?;
        Ok(frame::encode_frame(&payload))
    }

    /// 从未填充的 msgpack 载荷解码一个帧（由
    /// [`crate::FrameCodec::try_pop`] 生成）。
    pub fn from_payload(payload: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(payload)
    }

    /// 解码一个帧并校验其协议版本；如果版本错误，
    /// 则通过 [`TransportError::Other`] 返回
    /// 类似 [`crate::TransportError::Closed`] 的失败语义。
    pub fn from_payload_v(payload: &[u8]) -> Result<Self, crate::TransportError> {
        let frame = Self::from_payload(payload)?;
        if frame.ver != PROTOCOL_VERSION {
            return Err(crate::TransportError::other(format!(
                "protocol version {} != {}",
                frame.ver, PROTOCOL_VERSION
            )));
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_response_and_push_have_correct_ids() {
        let req = Frame::request(7, crate::Message::Ping);
        assert_eq!(req.id, 7);
        let res = Frame::response(7, crate::Message::Pong);
        assert_eq!(res.id, 7);
        let push = Frame::push(crate::Message::Pong);
        assert_eq!(push.id, 0);
    }

    #[test]
    fn frame_round_trips_through_wire_and_codec() {
        let frame = Frame::request(3, crate::Message::SendText {
            room: Some("general".into()),
            to: None,
            body: "hi".into(),
        });
        let wire = frame.to_wire().unwrap();

        // 完全像一次流读取那样，通过增量编解码器回放。
        let mut codec = crate::FrameCodec::new();
        codec.push(&wire[..5]).unwrap();
        codec.push(&wire[5..]).unwrap();
        let payload = codec.try_pop().unwrap().unwrap();
        let decoded = Frame::from_payload(&payload).unwrap();
        assert_eq!(decoded, frame);
        assert!(codec.try_pop().unwrap().is_none());
    }

    #[test]
    fn version_mismatch_is_detected() {
        let bad = Frame {
            ver: 999,
            id: 1,
            msg: crate::Message::Ping,
        };
        let wire = bad.to_wire().unwrap();
        let mut codec = crate::FrameCodec::new();
        codec.push(&wire).unwrap();
        let payload = codec.try_pop().unwrap().unwrap();
        assert!(Frame::from_payload_v(&payload).is_err());
    }
}
