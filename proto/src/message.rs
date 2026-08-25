//! Sinrana 消息集——强类型的信封载荷。
//!
//! 每个帧的主体都是一个 [`Message`]——一个使用 `rmp-serde` 以 msgpack
//! 序列化的内部标记枚举。`t` 标签携带一个稳定的字符串，如
//! `"chat.send"`；其余字段是每个操作的载荷。这是
//! 定义线上契约的唯一地方，并且被参考服务器和每个
//! 客户端逐字共享。
//!
//! 约定：
//! * 期望得到回复的请求通过信封 `id` 关联；匹配的
//!   响应（或 `sys.error`）携带相同的 `id`。
//! * 推送（服务端发起，例如 `chat.text` 和 `sys.notice`）不携带
//!   请求 `id`。
//! * `*_res` 变体是请求响应。

use serde::{Deserialize, Serialize};

/// 三级角色模型，从最低到最高权限。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Manager,
    Admin,
}

impl Role {
    /// 用于权限比较的层级（`Admin > Manager > User`）。
    pub fn rank(self) -> u8 {
        match self {
            Self::User => 0,
            Self::Manager => 1,
            Self::Admin => 2,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::User => "user",
            Self::Manager => "manager",
            Self::Admin => "admin",
        })
    }
}

/// 由 `chat.history` 返回的单条历史聊天消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryItem {
    pub seq: u64,
    pub room: String,
    pub from: String,
    pub body: String,
    pub ts: i64,
}

/// 完整的消息集。`t` 标签是稳定的判别器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Message {
    // ---- 认证 ------------------------------------------------------------
    #[serde(rename = "auth.login")]
    Login { username: String, password: String },
    #[serde(rename = "auth.login_res")]
    LoginRes {
        user_id: u64,
        username: String,
        roles: Vec<Role>,
        token: String,
    },
    #[serde(rename = "auth.register")]
    Register { username: String, password: String },
    #[serde(rename = "auth.register_res")]
    RegisterRes { username: String },
    #[serde(rename = "auth.resume")]
    Resume { token: String },
    #[serde(rename = "auth.resume_res")]
    ResumeRes { username: String, roles: Vec<Role> },
    #[serde(rename = "auth.logout")]
    Logout,

    // ---- 聊天 ------------------------------------------------------------
    /// 客户端向房间（公开）或用户（私聊）发送消息。
    #[serde(rename = "chat.send")]
    SendText {
        /// None => 私聊消息；Some(room) => 房间广播。
        room: Option<String>,
        /// 对于私聊消息，接收者用户名。
        to: Option<String>,
        body: String,
    },
    #[serde(rename = "chat.send_res")]
    SendTextRes { seq: u64, ts: i64 },
    /// 服务端推送：一条消息被投递到客户端所在的房间。
    #[serde(rename = "chat.text")]
    PushText {
        room: String,
        from: String,
        body: String,
        seq: u64,
        ts: i64,
    },
    /// 服务端推送：一条发给该客户端的私聊消息。
    #[serde(rename = "chat.private")]
    PushPrivate { from: String, body: String, ts: i64 },
    #[serde(rename = "chat.history")]
    History { room: String, limit: u32, before_seq: Option<u64> },
    #[serde(rename = "chat.history_res")]
    HistoryRes { room: String, messages: Vec<HistoryItem> },

    // ---- 房间 ------------------------------------------------------------
    #[serde(rename = "room.list")]
    RoomList,
    #[serde(rename = "room.list_res")]
    RoomListRes { rooms: Vec<String>, mine: Vec<String> },
    #[serde(rename = "room.create")]
    RoomCreate { name: String },
    #[serde(rename = "room.create_res")]
    RoomCreateRes { name: String },
    #[serde(rename = "room.join")]
    RoomJoin { name: String },
    #[serde(rename = "room.join_res")]
    RoomJoinRes { name: String },
    #[serde(rename = "room.leave")]
    RoomLeave { name: String },
    #[serde(rename = "room.leave_res")]
    RoomLeaveRes { name: String },
    #[serde(rename = "room.delete")]
    RoomDelete { name: String },
    #[serde(rename = "room.delete_res")]
    RoomDeleteRes { name: String },
    #[serde(rename = "room.members")]
    RoomMembers { name: String },
    #[serde(rename = "room.members_res")]
    RoomMembersRes { name: String, members: Vec<String> },

    // ---- 用户 / 管理 ----------------------------------------------------
    #[serde(rename = "user.online")]
    UserOnline,
    #[serde(rename = "user.online_res")]
    UserOnlineRes { users: Vec<String> },
    /// 服务端推送：在线用户清单发生变化（有成员加入/离开）。
    #[serde(rename = "user.manifest")]
    PushUserManifest { users: Vec<String> },
    #[serde(rename = "user.managers")]
    UserManagers,
    #[serde(rename = "user.managers_res")]
    UserManagersRes { managers: Vec<String> },
    #[serde(rename = "user.kick")]
    UserKick { target: String, reason: Option<String> },
    #[serde(rename = "user.kick_res")]
    UserKickRes { target: String },
    #[serde(rename = "admin.create")]
    AdminUserCreate { username: String, password: String, role: Role },
    #[serde(rename = "admin.create_res")]
    AdminUserCreateRes { username: String, role: Role },
    #[serde(rename = "admin.set_pwd")]
    AdminSetPwd { target: String, password: String },
    #[serde(rename = "admin.set_pwd_res")]
    AdminSetPwdRes { target: String },
    #[serde(rename = "admin.set_role")]
    AdminSetRole { target: String, role: Role },
    #[serde(rename = "admin.set_role_res")]
    AdminSetRoleRes { target: String, role: Role },
    #[serde(rename = "admin.delete")]
    AdminUserDelete { target: String },
    #[serde(rename = "admin.delete_res")]
    AdminUserDeleteRes { target: String },
    #[serde(rename = "admin.ban")]
    AdminUserBan { target: String, reason: Option<String> },
    #[serde(rename = "admin.ban_res")]
    AdminUserBanRes { target: String },
    #[serde(rename = "admin.restore")]
    AdminUserRestore { target: String },
    #[serde(rename = "admin.restore_res")]
    AdminUserRestoreRes { target: String },

    // ---- 系统 ------------------------------------------------------------
    #[serde(rename = "sys.ping")]
    Ping,
    #[serde(rename = "sys.pong")]
    Pong,
    #[serde(rename = "sys.server_info")]
    ServerInfo,
    #[serde(rename = "sys.server_info_res")]
    ServerInfoRes {
        protocol: u16,
        name: String,
        version: String,
        rooms: usize,
        online: usize,
    },
    /// 服务端推送：一条系统通知（踢出、封禁、修改密码、创建）。
    #[serde(rename = "sys.notice")]
    Notice { text: String, level: String },
    /// 携带结构化错误的服务端推送/响应。
    #[serde(rename = "sys.error")]
    Error {
        code: crate::ErrorCode,
        message: String,
        retryable: bool,
    },
}

impl Message {
    /// 该变体是否是服务端发起的推送（无需请求 id）。
    pub fn is_push(&self) -> bool {
        matches!(
            self,
            Message::PushText { .. }
                | Message::PushPrivate { .. }
                | Message::PushUserManifest { .. }
                | Message::Notice { .. }
        )
    }

    /// 该变体是否是对请求的响应。
    pub fn is_response(&self) -> bool {
        matches!(
            self,
            Message::LoginRes { .. }
                | Message::RegisterRes { .. }
                | Message::ResumeRes { .. }
                | Message::SendTextRes { .. }
                | Message::HistoryRes { .. }
                | Message::RoomListRes { .. }
                | Message::RoomCreateRes { .. }
                | Message::RoomJoinRes { .. }
                | Message::RoomLeaveRes { .. }
                | Message::RoomDeleteRes { .. }
                | Message::RoomMembersRes { .. }
                | Message::UserOnlineRes { .. }
                | Message::UserManagersRes { .. }
                | Message::UserKickRes { .. }
                | Message::AdminUserCreateRes { .. }
                | Message::AdminSetPwdRes { .. }
                | Message::AdminSetRoleRes { .. }
                | Message::AdminUserDeleteRes { .. }
                | Message::AdminUserBanRes { .. }
                | Message::AdminUserRestoreRes { .. }
                | Message::ServerInfoRes { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trips_through_msgpack() {
        let msg = Message::SendText {
            room: Some("general".into()),
            to: None,
            body: "hello".into(),
        };
        let bytes = rmp_serde::to_vec_named(&msg).unwrap();
        let decoded: Message = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn push_text_round_trips() {
        let msg = Message::PushText {
            room: "general".into(),
            from: "alice".into(),
            body: "hey".into(),
            seq: 42,
            ts: 1_700_000_000,
        };
        let bytes = rmp_serde::to_vec_named(&msg).unwrap();
        let decoded: Message = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(msg, decoded);
        assert!(decoded.is_push());
        assert!(!decoded.is_response());
    }

    #[test]
    fn role_ordering_is_low_to_high() {
        assert!(Role::User < Role::Manager);
        assert!(Role::Manager < Role::Admin);
    }

    #[test]
    fn variant_tag_is_stable() {
        // 确保我们在规范中发布的 `t` 标签保持固定。
        let res = Message::LoginRes {
            user_id: 1,
            username: "root".into(),
            roles: vec![Role::Admin],
            token: "abc".into(),
        };
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["t"], "auth.login_res");
    }
}
