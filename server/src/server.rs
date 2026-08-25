//! Sinrana 核心参考服务器：QUIC 端点、会话、房间、消息中继
//! 及管理操作。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use quinn::Incoming;
use sinrana_proto::{
    ErrorCode, Frame, HistoryItem, Message, ProtocolError, QuicTransport, Role,
    PROTOCOL_VERSION,
};
use tokio::sync::mpsc;

use crate::cert;
use crate::config::Config;
use crate::db::Db;

const HISTORY_CAP: usize = 512;
const OUTBOUND_QUEUE: usize = 1024;

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn role_from_str(s: &str) -> Role {
    match s {
        "admin" => Role::Admin,
        "manager" => Role::Manager,
        _ => Role::User,
    }
}

// ---------------------------------------------------------------------------
// 共享状态
// ---------------------------------------------------------------------------

pub(crate) struct OnlineUser {
    pub role: Role,
    pub tx: mpsc::Sender<Frame>,
}

pub(crate) struct Room {
    pub members: HashSet<String>,
}

#[derive(Clone)]
pub(crate) struct SessionInfo {
    pub username: String,
    pub role: Role,
}

pub(crate) struct State {
    pub cfg: std::sync::RwLock<Config>,
    pub db: Db,
    pub server_version: String,
    /// 服务器启动时使用的 TOML 配置文件路径（若有），这样 Web 端保存配置时
    /// 会写回同一个文件。
    pub config_path: Option<std::path::PathBuf>,
    pub online: Mutex<HashMap<String, OnlineUser>>,
    pub rooms: Mutex<HashMap<String, Room>>,
    pub sessions: Mutex<HashMap<String, SessionInfo>>,
    pub history: Mutex<HashMap<String, VecDeque<HistoryItem>>>,
    /// 将协议 `Message` 推送（房间文本、清单、通知）进行扇出，以便 WebUI
    /// 无需重复逻辑即可镜像 QUIC 客户端看到的内容。
    pub events: tokio::sync::broadcast::Sender<Message>,
    pub next_id: AtomicU64,
    pub next_seq: AtomicU64,
    /// 服务器启动时的墙钟时间（毫秒），用于运行时长上报。
    pub started_at: i64,
}

impl State {
    /// 克隆当前配置（读锁）。
    pub fn config(&self) -> Config {
        self.cfg.read().unwrap().clone()
    }

    /// 将 `incoming` 中可在运行时安全应用的部分热应用到在线配置，
    /// 保持仅重启生效的字段不变。返回生效后的配置。
    pub fn apply_runtime_config(&self, incoming: &Config) -> Config {
        let mut cfg = self.cfg.write().unwrap();
        cfg.default_room = incoming.default_room.clone();
        cfg.allow_register = incoming.allow_register;
        cfg.force_account = incoming.force_account;
        cfg.lock_server = incoming.lock_server;
        cfg.record_history = incoming.record_history;
        cfg.web_enabled = incoming.web_enabled;
        cfg.clone()
    }

    fn emit(&self, msg: Message) {
        let _ = self.events.send(msg);
    }

    pub fn online_users(&self) -> Vec<(String, Role)> {
        let online = self.online.lock().unwrap();
        let mut v: Vec<(String, Role)> = online
            .iter()
            .map(|(n, u)| (n.clone(), u.role))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    pub fn online_usernames(&self) -> Vec<String> {
        self.online.lock().unwrap().keys().cloned().collect()
    }

    pub fn is_online(&self, username: &str) -> bool {
        self.online.lock().unwrap().contains_key(username)
    }

    pub fn send_to(&self, username: &str, frame: Frame) {
        let online = self.online.lock().unwrap();
        if let Some(user) = online.get(username) {
            let _ = user.tx.try_send(frame);
        }
    }

    pub fn broadcast_manifest(&self) {
        let users = self.online_usernames();
        let mut list = users.clone();
        list.sort();
        let manifest = Message::PushUserManifest { users: list };
        self.emit(manifest.clone());
        let frame = Frame::push(manifest);
        for user in users {
            self.send_to(&user, frame.clone());
        }
    }

    pub fn broadcast_room(&self, room: &str, frame: Frame) {
        let members: Vec<String> = self
            .rooms
            .lock()
            .unwrap()
            .get(room)
            .map(|r| r.members.iter().cloned().collect())
            .unwrap_or_default();
        self.emit(frame.msg.clone());
        for m in members {
            self.send_to(&m, frame.clone());
        }
    }

    /// 分配 `seq`/`ts`，追加到历史记录，并向房间广播。这是唯一的排序权威，
    /// 使 QUIC 路径与 Web 终端共享一致顺序。
    /// 返回 `(seq, ts)`。
    pub fn broadcast_text(&self, from: &str, room: &str, body: &str) -> (u64, i64) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let ts = now_ms();
        let item = HistoryItem {
            seq,
            room: room.to_string(),
            from: from.to_string(),
            body: body.to_string(),
            ts,
        };
        self.push_history(room, item);
        self.broadcast_room(
            room,
            Frame::push(Message::PushText {
                room: room.to_string(),
                from: from.to_string(),
                body: body.to_string(),
                seq,
                ts,
            }),
        );
        (seq, ts)
    }

    pub fn room_names(&self) -> Vec<String> {
        self.rooms.lock().unwrap().keys().cloned().collect()
    }

    pub fn has_room(&self, room: &str) -> bool {
        self.rooms.lock().unwrap().contains_key(room)
    }

    pub fn room_members(&self, room: &str) -> Vec<String> {
        self.rooms
            .lock()
            .unwrap()
            .get(room)
            .map(|r| r.members.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn in_room(&self, room: &str, username: &str) -> bool {
        self.rooms
            .lock()
            .unwrap()
            .get(room)
            .map(|r| r.members.contains(username))
            .unwrap_or(false)
    }

    pub fn rooms_of(&self, username: &str) -> Vec<String> {
        let rooms = self.rooms.lock().unwrap();
        rooms
            .iter()
            .filter(|(_, r)| r.members.contains(username))
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn create_room(&self, room: &str, created_by: &str) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(r) = rooms.get_mut(room) {
            r.members.insert(created_by.to_string());
        } else {
            let mut members = HashSet::new();
            members.insert(created_by.to_string());
            rooms.insert(room.to_string(), Room { members });
        }
    }

    pub fn delete_room(&self, room: &str) {
        self.rooms.lock().unwrap().remove(room);
        self.history.lock().unwrap().remove(room);
    }

    pub fn add_to_room(&self, username: &str, room: &str) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(r) = rooms.get_mut(room) {
            r.members.insert(username.to_string());
        }
    }

    pub fn remove_from_room(&self, username: &str, room: &str) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(r) = rooms.get_mut(room) {
            r.members.remove(username);
        }
    }

    fn push_history(&self, room: &str, item: HistoryItem) {
        let mut history = self.history.lock().unwrap();
        let q = history.entry(room.to_string()).or_default();
        q.push_back(item);
        while q.len() > HISTORY_CAP {
            q.pop_front();
        }
    }

    pub fn register_online(&self, username: &str, role: Role, tx: mpsc::Sender<Frame>) {
        let mut online = self.online.lock().unwrap();
        online.insert(
            username.to_string(),
            OnlineUser { role, tx },
        );
    }

    pub fn unregister(&self, username: &str) {
        // 从所有房间中移除。
        let to_remove: Vec<String> = self
            .rooms
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, r)| r.members.contains(username))
            .map(|(name, _)| name.clone())
            .collect();
        for r in to_remove {
            self.remove_from_room(username, &r);
        }
        self.online.lock().unwrap().remove(username);
    }

    pub fn store_session(&self, token: &str, info: SessionInfo) {
        self.sessions.lock().unwrap().insert(token.to_string(), info);
    }

    pub fn lookup_session(&self, token: &str) -> Option<SessionInfo> {
        self.sessions.lock().unwrap().get(token).cloned()
    }

    pub fn history_for(&self, room: &str, limit: u32, before_seq: Option<u64>) -> Vec<HistoryItem> {
        let history = self.history.lock().unwrap();
        let mut items: Vec<HistoryItem> = history
            .get(room)
            .map(|q| {
                q.iter()
                    .filter(|i| before_seq.map_or(true, |b| i.seq < b))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if limit > 0 && items.len() > limit as usize {
            let start = items.len() - limit as usize;
            items = items.split_off(start);
        }
        items
    }
}

// ---------------------------------------------------------------------------
// 连接句柄
// ---------------------------------------------------------------------------

pub(crate) struct Handle {
    pub state: Arc<State>,
    pub self_tx: mpsc::Sender<Frame>,
}

impl Handle {
    fn send(&self, frame: Frame) {
        let _ = self.self_tx.try_send(frame);
    }
    fn reply(&self, req_id: u64, msg: Message) {
        self.send(Frame::response(req_id, msg));
    }
    fn error(&self, req_id: u64, err: ProtocolError) {
        self.send(Frame::error(req_id, err));
    }

    async fn handle_message(&self, identity: &mut Option<(String, Role)>, frame: Frame) {
        let req_id = frame.id;
        let msg = frame.msg;
        match msg {
            Message::Login { username, password } => {
                self.do_login(identity, req_id, &username, &password).await;
            }
            Message::Register { username, password } => {
                self.do_register(req_id, &username, &password).await;
            }
            Message::Resume { token } => {
                self.do_resume(identity, req_id, &token).await;
            }
            Message::Logout => {
                if let Some((name, _)) = identity.take() {
                    self.state.unregister(&name);
                    self.state.broadcast_manifest();
                    self.reply(req_id, Message::Notice {
                        text: "logged out".into(),
                        level: "info".into(),
                    });
                } else {
                    self.error(req_id, ProtocolError::new(ErrorCode::NotAuthenticated));
                }
            }
            // ---- 从这里开始需要会话 ----
            _ => {
                let Some((username, role)) = identity.clone() else {
                    self.error(req_id, ProtocolError::new(ErrorCode::NotAuthenticated));
                    return;
                };
                self.handle_authed(req_id, &username, role, msg).await;
            }
        }
    }

    async fn do_login(
        &self,
        identity: &mut Option<(String, Role)>,
        req_id: u64,
        username: &str,
        password: &str,
    ) {
        // 阻止同一连接重复登录。
        if identity.is_some() {
            self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("already logged in"));
            return;
        }
        let record = match self.state.db.get_user(username) {
            Ok(Some(r)) => r,
            Ok(None) => {
                self.error(req_id, ProtocolError::new(ErrorCode::AuthFailed));
                return;
            }
            Err(e) => {
                tracing::error!("db error: {e}");
                self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                return;
            }
        };
        if !crate::auth::verify_password(password, &record.password_hash) {
            self.error(req_id, ProtocolError::new(ErrorCode::AuthFailed));
            return;
        }
        if record.is_banned() {
            self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("banned"));
            return;
        }
        let role = role_from_str(&record.role);
        if self.state.config().lock_server && role != Role::Admin {
            self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("server locked"));
            return;
        }
        if self.state.is_online(username) {
            self.error(req_id, ProtocolError::new(ErrorCode::Conflict).with_message("already online"));
            return;
        }
        let user_id = self.state.next_id.fetch_add(1, Ordering::Relaxed);
        let token = crate::auth::generate_token();
        self.state.register_online(username, role, self.self_tx.clone());
        self.state.store_session(
            &token,
            SessionInfo {
                username: username.to_string(),
                role,
            },
        );
        self.state.create_room(&self.state.config().default_room, username);
        *identity = Some((username.to_string(), role));
        self.reply(req_id, Message::LoginRes {
            user_id,
            username: username.to_string(),
            roles: vec![role],
            token,
        });
        // 向刚登录的用户发送房间列表，然后广播用户清单。
        self.reply(0, Message::RoomListRes {
            rooms: self.state.room_names(),
            mine: self.state.rooms_of(username),
        });
        self.state.broadcast_manifest();
    }

    async fn do_register(&self, req_id: u64, username: &str, password: &str) {
        if !self.state.config().allow_register {
            self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("registration disabled"));
            return;
        }
        if username.is_empty() || username.len() > 20 || username == "Server" || username == "admin" {
            self.error(req_id, ProtocolError::new(ErrorCode::BadRequest).with_message("invalid username"));
            return;
        }
        if password.len() < 4 {
            self.error(req_id, ProtocolError::new(ErrorCode::BadRequest).with_message("password too short"));
            return;
        }
        if self.state.db.get_user(username).ok().flatten().is_some() {
            self.error(req_id, ProtocolError::new(ErrorCode::UserExists));
            return;
        }
        let hash = match crate::auth::hash_password(password) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("hash error: {e}");
                self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                return;
            }
        };
        if let Err(e) = self.state.db.create_user(username, &hash, "user") {
            tracing::error!("db error: {e}");
            self.error(req_id, ProtocolError::new(ErrorCode::Internal));
            return;
        }
        self.reply(req_id, Message::RegisterRes {
            username: username.to_string(),
        });
    }

    async fn do_resume(&self, identity: &mut Option<(String, Role)>, req_id: u64, token: &str) {
        if identity.is_some() {
            self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("already logged in"));
            return;
        }
        let Some(info) = self.state.lookup_session(token) else {
            self.error(req_id, ProtocolError::new(ErrorCode::NotAuthenticated));
            return;
        };
        if !self.state.is_online(&info.username) {
            self.error(req_id, ProtocolError::new(ErrorCode::NotAuthenticated));
            return;
        }
        *identity = Some((info.username.clone(), info.role));
        self.reply(req_id, Message::ResumeRes {
            username: info.username,
            roles: vec![info.role],
        });
    }

    async fn handle_authed(&self, req_id: u64, username: &str, role: Role, msg: Message) {
        match msg {
            Message::SendText { room, to, body } => {
                self.do_send_text(req_id, username, &room, &to, &body).await;
            }
            Message::History { room, limit, before_seq } => {
                let is_in = self.state.in_room(&room, username) || self.state.has_room(&room);
                if !is_in {
                    self.error(req_id, ProtocolError::new(ErrorCode::RoomNotFound));
                    return;
                }
                self.reply(req_id, Message::HistoryRes {
                    room: room.clone(),
                    messages: self.state.history_for(&room, limit, before_seq),
                });
            }
            Message::RoomList => {
                self.reply(req_id, Message::RoomListRes {
                    rooms: self.state.room_names(),
                    mine: self.state.rooms_of(username),
                });
            }
            Message::RoomCreate { name } => {
                if role.rank() < Role::Manager.rank() {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                if self.state.has_room(&name) || self.state.is_online(&name) {
                    self.error(req_id, ProtocolError::new(ErrorCode::Conflict).with_message("room exists"));
                    return;
                }
                self.state.create_room(&name, username);
                self.state.add_to_room(username, &name);
                let _ = self.state.db.create_room(&name);
                self.reply(req_id, Message::RoomCreateRes { name: name.clone() });
                self.broadcast_room_notice(&name, format!("{username} created room {name}"));
            }
            Message::RoomJoin { name } => {
                if !self.state.has_room(&name) {
                    self.error(req_id, ProtocolError::new(ErrorCode::RoomNotFound));
                    return;
                }
                self.state.add_to_room(username, &name);
                self.reply(req_id, Message::RoomJoinRes { name: name.clone() });
                self.broadcast_room_notice(&name, format!("{username} joined {name}"));
            }
            Message::RoomLeave { name } => {
                self.state.remove_from_room(username, &name);
                self.reply(req_id, Message::RoomLeaveRes { name: name.clone() });
                self.broadcast_room_notice(&name, format!("{username} left {name}"));
            }
            Message::RoomDelete { name } => {
                if role != Role::Admin {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                if !self.state.has_room(&name) {
                    self.error(req_id, ProtocolError::new(ErrorCode::RoomNotFound));
                    return;
                }
                self.state.delete_room(&name);
                let _ = self.state.db.delete_room(&name);
                self.reply(req_id, Message::RoomDeleteRes { name: name.clone() });
            }
            Message::RoomMembers { name } => {
                if !self.state.has_room(&name) {
                    self.error(req_id, ProtocolError::new(ErrorCode::RoomNotFound));
                    return;
                }
                let mut members = self.state.room_members(&name);
                members.sort();
                self.reply(req_id, Message::RoomMembersRes { name, members });
            }
            Message::UserOnline => {
                let mut users = self.state.online_usernames();
                users.sort();
                self.reply(req_id, Message::UserOnlineRes { users });
            }
            Message::UserManagers => {
                let managers: Vec<String> = {
                    let online = self.state.online.lock().unwrap();
                    online
                        .iter()
                        .filter(|(_, u)| u.role.rank() >= Role::Manager.rank())
                        .map(|(n, _)| n.clone())
                        .collect()
                };
                self.reply(req_id, Message::UserManagersRes { managers });
            }
            Message::UserKick { target, reason } => {
                if role.rank() < Role::Manager.rank() {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                if !self.state.is_online(&target) {
                    self.error(req_id, ProtocolError::new(ErrorCode::NotFound).with_message("target not online"));
                    return;
                }
                if target == "admin" || target == username {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("cannot kick self/admin"));
                    return;
                }
                self.state.send_to(
                    &target,
                    Frame::push(Message::Notice {
                        text: format!(
                            "kicked from server{}",
                            reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()
                        ),
                        level: "warn".into(),
                    }),
                );
                self.state.unregister(&target);
                self.state.broadcast_manifest();
                self.reply(req_id, Message::UserKickRes { target: target.clone() });
            }
            Message::AdminUserCreate { username: cname, password, role: new_role } => {
                if role != Role::Admin {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                if self.state.db.get_user(&cname).ok().flatten().is_some() || self.state.is_online(&cname) || cname == "Server" {
                    self.error(req_id, ProtocolError::new(ErrorCode::UserExists));
                    return;
                }
                let hash = match crate::auth::hash_password(&password) {
                    Ok(h) => h,
                    Err(_) => {
                        self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                        return;
                    }
                };
                if let Err(e) = self.state.db.create_user(&cname, &hash, &new_role.to_string()) {
                    tracing::error!("db: {e}");
                    self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                    return;
                }
                self.reply(req_id, Message::AdminUserCreateRes { username: cname, role: new_role });
            }
            Message::AdminSetPwd { target, password } => {
                if role != Role::Admin {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                let hash = match crate::auth::hash_password(&password) {
                    Ok(h) => h,
                    Err(_) => {
                        self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                        return;
                    }
                };
                if self.state.db.set_password(&target, &hash).is_err() {
                    self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                    return;
                }
                self.reply(req_id, Message::AdminSetPwdRes { target });
            }
            Message::AdminSetRole { target, role: new_role } => {
                if role != Role::Admin {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                if target == "admin" || self.state.db.set_role(&target, &new_role.to_string()).is_err() {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("cannot change admin role"));
                    return;
                }
                // 若用户在线，则更新其实时角色。
                {
                    let mut online = self.state.online.lock().unwrap();
                    if let Some(u) = online.get_mut(&target) {
                        u.role = new_role;
                    }
                }
                self.reply(req_id, Message::AdminSetRoleRes { target, role: new_role });
            }
            Message::AdminUserDelete { target } => {
                if role != Role::Admin {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                if target == "admin" || target == username {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("cannot delete self/admin"));
                    return;
                }
                if self.state.db.delete_user(&target).is_err() {
                    self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                    return;
                }
                if self.state.is_online(&target) {
                    self.state.send_to(&target, Frame::push(Message::Notice {
                        text: "your account was deleted".into(),
                        level: "warn".into(),
                    }));
                    self.state.unregister(&target);
                    self.state.broadcast_manifest();
                }
                self.reply(req_id, Message::AdminUserDeleteRes { target });
            }
            Message::AdminUserBan { target, reason } => {
                if role != Role::Admin {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                if target == "admin" || target == username {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden).with_message("cannot ban self/admin"));
                    return;
                }
                if self.state.db.set_banned(&target, true).is_err() {
                    self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                    return;
                }
                if self.state.is_online(&target) {
                    self.state.send_to(&target, Frame::push(Message::Notice {
                        text: format!(
                            "banned{}",
                            reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()
                        ),
                        level: "warn".into(),
                    }));
                    self.state.unregister(&target);
                    self.state.broadcast_manifest();
                }
                self.reply(req_id, Message::AdminUserBanRes { target });
            }
            Message::AdminUserRestore { target } => {
                if role != Role::Admin {
                    self.error(req_id, ProtocolError::new(ErrorCode::Forbidden));
                    return;
                }
                if self.state.db.set_banned(&target, false).is_err() {
                    self.error(req_id, ProtocolError::new(ErrorCode::Internal));
                    return;
                }
                self.reply(req_id, Message::AdminUserRestoreRes { target });
            }
            Message::ServerInfo => {
                self.reply(req_id, Message::ServerInfoRes {
                    protocol: PROTOCOL_VERSION,
                    name: "Sinrana".into(),
                    version: self.state.server_version.clone(),
                    rooms: self.state.room_names().len(),
                    online: self.state.online_usernames().len(),
                });
            }
            Message::Ping => self.reply(req_id, Message::Pong),
            other => {
                self.error(req_id, ProtocolError::new(ErrorCode::BadRequest).with_message(format!("unexpected message: {other:?}")));
            }
        }
    }

    fn broadcast_room_notice(&self, room: &str, text: String) {
        self.state
            .broadcast_room(room, Frame::push(Message::Notice { text, level: "info".into() }));
    }

    async fn do_send_text(&self, req_id: u64, username: &str, room: &Option<String>, to: &Option<String>, body: &str) {
        if body.is_empty() {
            self.error(req_id, ProtocolError::new(ErrorCode::BadRequest).with_message("empty message"));
            return;
        }
        match (room, to) {
            (Some(room), _) => {
                if !self.state.in_room(room, username) {
                    self.error(req_id, ProtocolError::new(ErrorCode::NotInRoom));
                    return;
                }
                let (seq, ts) = self.state.broadcast_text(username, room, body);
                self.reply(req_id, Message::SendTextRes { seq, ts });
            }
            (None, Some(target)) => {
                if !self.state.is_online(target) {
                    self.error(req_id, ProtocolError::new(ErrorCode::NotFound).with_message("recipient not online"));
                    return;
                }
                self.state.send_to(target, Frame::push(Message::PushPrivate {
                    from: username.to_string(),
                    body: body.to_string(),
                    ts: now_ms(),
                }));
                self.reply(req_id, Message::SendTextRes { seq: 0, ts: now_ms() });
            }
            _ => {
                self.error(req_id, ProtocolError::new(ErrorCode::BadRequest).with_message("specify room or recipient"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 服务器
// ---------------------------------------------------------------------------

pub struct Server {
    pub(crate) state: Arc<State>,
    cert_der: Vec<u8>,
    endpoint: quinn::Endpoint,
    server_name: String,
    server_version: String,
}

impl Server {
    /// 构建 QUIC 端点（如需要会生成自签名证书），并返回可供
    /// [`Server::run`] 使用的 `Server`。WebUI 仍会启动（若已启用），
    /// 但配置保存没有目标文件（`config_path = None`）。
    pub async fn start(cfg: Config) -> Result<Self> {
        Self::start_with_config(cfg, None).await
    }

    /// 与 [`Server::start`] 类似，但会记住服务器启动时使用的 TOML 配置路径，
    /// 这样 Web 端保存配置时会写回该文件。
    pub async fn start_with_config(cfg: Config, config_path: Option<std::path::PathBuf>) -> Result<Self> {
        // 选择 ring 加密提供程序，以避免 rustls/quinn 因默认值不明确而 panic
        // （`ring` 和 `aws-lc-rs` 可能同时被编译）。
        let _ = rustls::crypto::ring::default_provider().install_default();

        let server_version = env!("CARGO_PKG_VERSION").to_string();
        let db = Db::open(&cfg.db_path).context("open db")?;
        let _created = db.ensure_admin("admin").context("seed admin")?;

        let (events_tx, _) = tokio::sync::broadcast::channel::<Message>(256);
        let state = State {
            cfg: std::sync::RwLock::new(cfg.clone()),
            db,
            server_version: server_version.clone(),
            config_path,
            online: Mutex::new(HashMap::new()),
            rooms: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            history: Mutex::new(HashMap::new()),
            events: events_tx,
            next_id: AtomicU64::new(1),
            next_seq: AtomicU64::new(1),
            started_at: now_ms(),
        };
        // 确保默认房间存在。
        state.create_room(&cfg.default_room, "system");
        let state = Arc::new(state);

        // TLS 配置。
        let (cert_der, key) = match (&cfg.cert_file, &cfg.key_file) {
            // 配置/CLI 指定了证书路径：文件存在则读取，缺失则就地生成并落盘复用。
            (Some(cert_file), Some(key_file)) => {
                cert::load_or_generate(cert_file, key_file).context("加载或生成证书")?
            }
            _ => {
                let (cd, key) = cert::self_signed().context("生成自签名证书")?;
                (cd.as_ref().to_vec(), key)
            }
        };
        let cert_chain = vec![rustls::pki_types::CertificateDer::from(cert_der.clone())];
        let crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)
            .context("build rustls server config")?;
        let server_config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(crypto).context("quic server config")?,
        ));
        let endpoint = quinn::Endpoint::server(server_config, cfg.bind_addr).context("bind quic endpoint")?;

        Ok(Server {
            state,
            cert_der,
            endpoint,
            server_name: "Sinrana".into(),
            server_version,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    pub fn web_enabled(&self) -> bool {
        self.state.config().web_enabled
    }

    pub fn web_addr(&self) -> SocketAddr {
        self.state.config().web_addr
    }

    pub fn online_count(&self) -> usize {
        self.state.online_usernames().len()
    }

    /// 持续接受 QUIC 连接，并在进程运行期间（若已启用）于后台任务中
    /// 提供管理 WebUI。
    pub async fn run(self) -> Result<()> {
        // 在进入接受循环前启动 WebUI，使两个界面同时启动。
        // Web 服务器只需要 state 的一个克隆。
        #[cfg(feature = "web")]
        {
            let cfg = self.state.config();
            if cfg.web_enabled {
                let state = self.state.clone();
                let web_addr = cfg.web_addr;
                let cert_der = self.cert_der.clone();
                let server_version = self.server_version.clone();
                let server_name = self.server_name.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::web::serve(state, web_addr, cert_der, server_version, server_name).await {
                        tracing::error!("web ui failed: {e}");
                    }
                });
            }
        }

        tracing::info!("Sinrana server listening on {}", self.endpoint.local_addr()?);
        while let Some(conn) = self.endpoint.accept().await {
            let state = self.state.clone();
            let server_name = self.server_name.clone();
            tokio::spawn(async move {
                if let Err(e) = conn_loop(&state, &server_name, conn).await {
                    tracing::debug!("connection ended: {e}");
                }
            });
        }
        Ok(())
    }

    /// 关闭端点。
    pub fn close(&self) {
        self.endpoint.close(0u8.into(), b"server shutting down");
    }
}

async fn conn_loop(state: &Arc<State>, _server_name: &str, conn: Incoming) -> Result<()> {
    let connection = conn.await?;
    // 接受客户端唯一的双向流。
    let (send, recv) = connection
        .accept_bi()
        .await
        .map_err(|e| anyhow!("accept stream: {e}"))?;
    let transport = QuicTransport::new(send, recv);
    let (send_half, recv_half) = transport.split();
    let (tx, mut rx) = mpsc::channel::<Frame>(OUTBOUND_QUEUE);

    let writer = tokio::spawn(async move {
        let mut sh = send_half;
        while let Some(frame) = rx.recv().await {
            if sh.send(&frame).await.is_err() {
                break;
            }
        }
    });

    let handle = Handle {
        state: state.clone(),
        self_tx: tx,
    };
    let mut identity: Option<(String, Role)> = None;
    let result = reader_loop(&handle, &mut identity, recv_half).await;

    // 断开连接时清理。
    if let Some((username, _)) = &identity {
        handle.state.unregister(username);
        handle.state.broadcast_manifest();
    }
    let _ = writer.await;
    result
}

async fn reader_loop(handle: &Handle, identity: &mut Option<(String, Role)>, mut recv: sinrana_proto::RecvHalf) -> Result<()> {
    loop {
        let frame = match recv.recv().await {
            Ok(f) => f,
            Err(sinrana_proto::TransportError::Closed) => return Ok(()),
            Err(e) => return Err(anyhow!("recv: {e}")),
        };
        handle.handle_message(identity, frame).await;
    }
}
