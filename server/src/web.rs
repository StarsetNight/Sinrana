//! 管理 WebUI：提供在 QUIC 端点旁运行的进程内 HTTP + WebSocket
//! 界面。
//!
//! WebUI 有意与 msgpack/QUIC 协议分开：它通过 HTTP/WebSocket 与浏览器
//! 通信并使用 JSON。处理程序读取并修改 QUIC 服务器拥有的同一个
//! [`crate::server::State`]，因此 Web 发起的聊天、管理命令和配置编辑
//! 都由同一个排序权威应用，
//! QUIC 客户端会立即观察到它们。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State as AxState};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sinrana_proto::{Frame, Message, Role, PROTOCOL_VERSION};

use crate::server::State;

/// 共享的 Web 处理程序状态：服务器 `State` 加上已签发的
/// 管理会话令牌集合。
#[derive(Clone)]
pub struct WebState {
    pub(crate) state: Arc<State>,
    pub(crate) tokens: Arc<Mutex<HashMap<String, String>>>, // token -> username
}

/// 构建 axum 0.8 WebSocket 接受的文本帧（`Message::Text` 包装
/// `Utf8Bytes`，因此 `String` 必须转换）。
fn ws_text(s: String) -> WsMessage {
    WsMessage::Text(s.into())
}

/// 在 `web_addr` 上提供 WebUI 服务，直到进程退出。
pub(crate) async fn serve(
    state: Arc<State>,
    web_addr: SocketAddr,
    _cert_der: Vec<u8>,
    _server_version: String,
    _server_name: String,
) -> anyhow::Result<()> {
    let web = WebState {
        state,
        tokens: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/login", post(api_login))
        .route("/api/info", get(api_info))
        .route("/api/online", get(api_online))
        .route("/api/rooms", get(api_rooms))
        .route("/api/history", get(api_history))
        .route("/api/config", get(api_config))
        .route("/api/config", put(api_config_save))
        .route("/api/events", get(api_events))
        .route("/ws", get(ws_terminal))
        .with_state(web);

    let listener = tokio::net::TcpListener::bind(web_addr).await?;
    tracing::info!("WebUI listening on http://{web_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("web/index.html"))
}

// ---------------------------------------------------------------------------
// 认证
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginRes {
    token: String,
    username: String,
}

async fn api_login(AxState(web): AxState<WebState>, Json(req): Json<LoginReq>) -> impl IntoResponse {
    let state = &web.state;
    let rec = match state.db.get_user(&req.username) {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::UNAUTHORIZED, Json(json!({"error":"invalid credentials"}))).into_response(),
        Err(e) => {
            tracing::error!("db: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal"}))).into_response();
        }
    };
    if !crate::auth::verify_password(&req.password, &rec.password_hash) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"invalid credentials"}))).into_response();
    }
    if rec.is_banned() {
        return (StatusCode::FORBIDDEN, Json(json!({"error":"banned"}))).into_response();
    }
    if crate::server::role_from_str(&rec.role) != Role::Admin {
        return (StatusCode::FORBIDDEN, Json(json!({"error":"admin only"}))).into_response();
    }
    let token = crate::auth::generate_token();
    web.tokens.lock().unwrap().insert(token.clone(), rec.username.clone());
    (StatusCode::OK, Json(LoginRes { token, username: rec.username })).into_response()
}

/// 校验 bearer 令牌，若会话仍处于激活状态且仍为管理员，
/// 则返回用户名。
fn authorized(web: &WebState, token: &str) -> Option<String> {
    let username = web.tokens.lock().unwrap().get(token).cloned()?;
    let rec = web.state.db.get_user(&username).ok().flatten()?;
    if rec.is_banned() || crate::server::role_from_str(&rec.role) != Role::Admin {
        return None;
    }
    Some(username)
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// 监控端点
// ---------------------------------------------------------------------------

async fn api_info(AxState(web): AxState<WebState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let Some(_u) = bearer_token(&headers).and_then(|t| authorized(&web, &t)) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"}))).into_response();
    };
    let state = &web.state;
    let cfg = state.config();
    let uptime_ms = crate::server::now_ms() - state.started_at;
    (
        StatusCode::OK,
        Json(json!({
            "protocol": PROTOCOL_VERSION,
            "name": "Sinrana",
            "version": state.server_version.clone(),
            "rooms": state.room_names().len(),
            "online": state.online_usernames().len(),
            "uptime_ms": uptime_ms,
            "server_seq": state.next_seq.load(Ordering::Relaxed),
            "bind_addr": cfg.bind_addr.to_string(),
            "web_addr": cfg.web_addr.to_string(),
            "web_enabled": cfg.web_enabled,
            "default_room": cfg.default_room,
            "db_path": cfg.db_path.display().to_string(),
        })),
    )
        .into_response()
}

async fn api_online(AxState(web): AxState<WebState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let Some(_u) = bearer_token(&headers).and_then(|t| authorized(&web, &t)) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"}))).into_response();
    };
    let users = web
        .state
        .online_users()
        .into_iter()
        .map(|(n, r)| json!({"username": n, "role": r.to_string()}))
        .collect::<Vec<_>>();
    (StatusCode::OK, Json(json!({"users": users}))).into_response()
}

async fn api_rooms(AxState(web): AxState<WebState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let Some(_u) = bearer_token(&headers).and_then(|t| authorized(&web, &t)) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"}))).into_response();
    };
    let state = &web.state;
    let mut rooms: Vec<Value> = state
        .room_names()
        .into_iter()
        .map(|name| {
            let members = state.room_members(&name);
            json!({"name": name, "members": members.len(), "member_names": members})
        })
        .collect();
    rooms.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    (StatusCode::OK, Json(json!({"rooms": rooms}))).into_response()
}

#[derive(Deserialize)]
struct HistoryQuery {
    room: Option<String>,
    limit: Option<u32>,
}

async fn api_history(
    AxState(web): AxState<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> impl IntoResponse {
    let Some(_u) = bearer_token(&headers).and_then(|t| authorized(&web, &t)) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"}))).into_response();
    };
    let room = q.room.unwrap_or_default();
    if room.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"room required"}))).into_response();
    }
    let items = web.state.history_for(&room, q.limit.unwrap_or(50), None);
    (StatusCode::OK, Json(json!({"room": room, "messages": items}))).into_response()
}

// ---------------------------------------------------------------------------
// 配置端点
// ---------------------------------------------------------------------------

async fn api_config(AxState(web): AxState<WebState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let Some(_u) = bearer_token(&headers).and_then(|t| authorized(&web, &t)) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"}))).into_response();
    };
    let cfg = web.state.config();
    let runtime: Vec<String> = cfg.runtime_fields().iter().map(|s| s.to_string()).collect();
    (
        StatusCode::OK,
        Json(json!({"config": cfg, "runtime_fields": runtime})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ConfigBody {
    config: crate::config::Config,
}

async fn api_config_save(
    AxState(web): AxState<WebState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ConfigBody>,
) -> impl IntoResponse {
    let Some(_u) = bearer_token(&headers).and_then(|t| authorized(&web, &t)) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"}))).into_response();
    };
    let incoming = body.config;
    if incoming.default_room.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"default_room must not be empty"}))).into_response();
    }

    // 将在运行时安全的部分热应用到在线配置。
    web.state.apply_runtime_config(&incoming);

    // 将完整配置持久化回服务器启动时使用的同一个文件；
    // 若没有配置文件（例如仅 CLI 启动），则跳过写入。
    if let Some(cfg_path) = &web.state.config_path {
        if let Err(e) = incoming.save(cfg_path) {
            tracing::error!("save config: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"failed to save config"}))).into_response();
        }
    }

    let runtime: Vec<String> = incoming.runtime_fields().iter().map(|s| s.to_string()).collect();
    (StatusCode::OK, Json(json!({"saved": true, "runtime_fields": runtime}))).into_response()
}

// ---------------------------------------------------------------------------
// 实时事件流（SSE）+ 终端（WebSocket）
// ---------------------------------------------------------------------------

async fn api_events(AxState(web): AxState<WebState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let Some(_u) = bearer_token(&headers).and_then(|t| authorized(&web, &t)) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"}))).into_response();
    };
    let rx = web.state.events.subscribe();

    let stream = futures_util::stream::unfold(
        rx,
        |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        let Ok(data) = serde_json::to_string(&msg) else {
                            continue;
                        };
                        return Some((
                            Ok::<_, std::convert::Infallible>(Event::default().data(data)),
                            rx,
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        return Some((
                            Ok::<_, std::convert::Infallible>(
                                Event::default().comment("lagged; reconnect").event("lagged").data("true"),
                            ),
                            rx,
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );

    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// 为 WebSocket 终端认证管理员。令牌以查询参数传递（`?token=...`），
/// 因为浏览器无法在 WS 上设置 Authorization。
async fn ws_terminal(
    AxState(web): AxState<WebState>,
    query: Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = query.0.token.clone().unwrap_or_default();
    let Some(username) = authorized(&web, &token) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"}))).into_response();
    };
    ws.on_upgrade(move |socket| handle_terminal(socket, web, username))
}

#[derive(Deserialize)]
struct WsQuery {
    token: Option<String>,
}

fn now_ms() -> i64 {
    crate::server::now_ms()
}

async fn handle_terminal(mut socket: WebSocket, web: WebState, username: String) {
    let mut rx = web.state.events.subscribe();

    let hello = json!({
        "type": "hello",
        "username": username,
        "info": {
            "protocol": PROTOCOL_VERSION,
            "name": "Sinrana",
            "version": web.state.server_version.clone(),
            "rooms": web.state.room_names().len(),
            "online": web.state.online_usernames().len(),
        },
        "rooms": web.state.room_names(),
    });
    let _ = socket.send(ws_text(hello.to_string())).await;

    loop {
        tokio::select! {
            biased;
            line = socket.recv() => {
                let Some(Ok(frame)) = line else { break };
                match frame {
                    WsMessage::Text(txt) => on_client_message(&web, &mut socket, txt.as_str()).await,
                    WsMessage::Close(_) => break,
                    _ => {}
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(msg) => {
                        let _ = socket.send(ws_text(
                            json!({"type":"event","payload":msg}).to_string()
                        )).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let _ = socket.send(ws_text(
                            json!({"type":"notice","text":"event stream lagged","level":"warn"}).to_string()
                        )).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn on_client_message(web: &WebState, socket: &mut WebSocket, text: &str) {
    let state = &web.state;
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            let _ = socket.send(ws_text(json!({"type":"error","text":format!("bad json: {e}")}).to_string())).await;
            return;
        }
    };
    match parsed["type"].as_str() {
        Some("chat") => {
            let body = parsed["body"].as_str().unwrap_or_default();
            if body.is_empty() {
                let _ = socket.send(ws_text(json!({"type":"error","text":"empty message"}).to_string())).await;
                return;
            }
            let room = parsed["room"].as_str().unwrap_or(&state.config().default_room).to_string();
            if !state.has_room(&room) {
                let _ = socket.send(ws_text(json!({"type":"error","text":"room not found"}).to_string())).await;
                return;
            }
            let (seq, ts) = state.broadcast_text("Server", &room, body);
            let _ = socket.send(ws_text(json!({"type":"chat_ack","seq":seq,"ts":ts}).to_string())).await;
        }
        Some("cmd") => {
            let line = parsed["line"].as_str().unwrap_or_default().trim().to_string();
            let _ = dispatch_command(web, socket, &line).await;
        }
        Some("ping") => {
            let _ = socket.send(ws_text(json!({"type":"pong"}).to_string())).await;
        }
        _ => {
            let _ = socket.send(ws_text(json!({"type":"error","text":"unknown message type"}).to_string())).await;
        }
    }
}

/// 执行终端命令并在 socket 上回复。镜像 CLI 的管理命令集，
/// Web 终端以 root（`Server`）身份运行。
async fn dispatch_command(web: &WebState, socket: &mut WebSocket, line: &str) -> Result<(), ()> {
    let state = &web.state;
    let (cmd, rest) = line
        .split_once(' ')
        .map(|(a, b)| (a, b.trim().to_string()))
        .unwrap_or((line, String::new()));

    let reply = match cmd {
        "/help" => {
            json!({"type":"notice","level":"info","text":"/kick <u>  /ban <u> [r]  /restore <u>  /setrole <u> <role>  /setpwd <u> <p>  /create <room>  /delete <room>  /broadcast <text>  /createuser <u> <p> <role>  /info  /rooms  /online"})
        }
        "/info" => {
            let cfg = state.config();
            json!({"type":"info","rooms":state.room_names().len(),"online":state.online_usernames().len(),"uptime_ms":now_ms()-state.started_at,"server_seq":state.next_seq.load(Ordering::Relaxed),"bind_addr":cfg.bind_addr.to_string()})
        }
        "/rooms" => json!({"type":"rooms","rooms":state.room_names()}),
        "/online" => json!({"type":"online","users":state.online_users().iter().map(|(n,r)| json!({"username":n,"role":r.to_string()})).collect::<Vec<_>>()}),
        "/kick" => {
            if rest.is_empty() { json!({"type":"error","text":"usage: /kick <user>"}) }
            else {
                let target = rest.trim();
                if !state.is_online(target) { json!({"type":"error","text":"target not online"}) }
                else if target == "admin" { json!({"type":"error","text":"cannot kick admin"}) }
                else {
                    state.send_to(target, Frame::push(Message::Notice{ text: format!("kicked from server by web admin"), level:"warn".into() }));
                    state.unregister(target);
                    state.broadcast_manifest();
                    json!({"type":"notice","level":"info","text":format!("kicked {target}")})
                }
            }
        }
        "/ban" => {
            let (target, reason) = rest.split_once(' ').map(|(t, r)| (t.trim().to_string(), Some(r.trim().to_string()))).unwrap_or((rest.trim().to_string(), None));
            if target.is_empty() || target == "admin" { json!({"type":"error","text":"invalid target"}) }
            else if state.db.set_banned(&target, true).is_err() { json!({"type":"error","text":"db error"}) }
            else {
                if state.is_online(&target) {
                    state.send_to(&target, Frame::push(Message::Notice{ text: format!("banned from server{}", reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()), level:"warn".into() }));
                    state.unregister(&target);
                    state.broadcast_manifest();
                }
                json!({"type":"notice","level":"info","text":format!("banned {target}")})
            }
        }
        "/restore" => {
            if rest.is_empty() { json!({"type":"error","text":"usage: /restore <user>"}) }
            else {
                let target = rest.trim();
                if state.db.set_banned(target, false).is_err() { json!({"type":"error","text":"db error"}) }
                else { json!({"type":"notice","level":"info","text":format!("restored {target}")}) }
            }
        }
        "/setrole" => {
            let (target, role) = rest.split_once(' ').map(|(t, r)| (t.trim().to_string(), r.trim().to_string())).unwrap_or_default();
            if target.is_empty() || role.is_empty() { json!({"type":"error","text":"usage: /setrole <user> <role>"}) }
            else if target == "admin" || (role != "user" && role != "manager" && role != "admin") { json!({"type":"error","text":"invalid target/role"}) }
            else {
                let new_role = crate::server::role_from_str(&role);
                if state.db.set_role(&target, &role).is_err() { json!({"type":"error","text":"db error"}) }
                else {
                    {
                        let mut online = state.online.lock().unwrap();
                        if let Some(u) = online.get_mut(&target) { u.role = new_role; }
                    }
                    json!({"type":"notice","level":"info","text":format!("{target} role -> {role}")})
                }
            }
        }
        "/setpwd" => {
            let (target, pass) = rest.split_once(' ').map(|(t, r)| (t.trim().to_string(), r.trim().to_string())).unwrap_or_default();
            if target.is_empty() || pass.is_empty() { json!({"type":"error","text":"usage: /setpwd <user> <password>"}) }
            else {
                match crate::auth::hash_password(&pass) {
                    Ok(h) if state.db.set_password(&target, &h).is_ok() => json!({"type":"notice","level":"info","text":format!("password updated for {target}")}),
                    _ => json!({"type":"error","text":"failed to set password"}),
                }
            }
        }
        "/create" => {
            if rest.is_empty() { json!({"type":"error","text":"usage: /create <room>"}) }
            else if state.has_room(&rest) || state.is_online(&rest) { json!({"type":"error","text":"room exists"}) }
            else {
                state.create_room(&rest, "Server");
                let _ = state.db.create_room(&rest);
                json!({"type":"notice","level":"info","text":format!("created room {rest}")})
            }
        }
        "/delete" => {
            if rest.is_empty() { json!({"type":"error","text":"usage: /delete <room>"}) }
            else if !state.has_room(&rest) { json!({"type":"error","text":"room not found"}) }
            else {
                state.delete_room(&rest);
                let _ = state.db.delete_room(&rest);
                json!({"type":"notice","level":"info","text":format!("deleted room {rest}")})
            }
        }
        "/createuser" => {
            let it = rest.split_whitespace();
            let parts: Vec<&str> = it.collect();
            if parts.len() < 3 {
                json!({"type":"error","text":"usage: /createuser <user> <password> <role>"})
            } else {
                let u = parts[0];
                let p = parts[1];
                let r = parts[2];
                if state.db.get_user(u).ok().flatten().is_some() { json!({"type":"error","text":"user exists"}) }
                else {
                    match crate::auth::hash_password(p) {
                        Ok(h) if state.db.create_user(u, &h, r).is_ok() => json!({"type":"notice","level":"info","text":format!("created user {u} as {r}")}),
                        _ => json!({"type":"error","text":"failed to create user"}),
                    }
                }
            }
        }
        "/broadcast" => {
            if rest.is_empty() { json!({"type":"error","text":"usage: /broadcast <text>"}) }
            else {
                for room in state.room_names() {
                    state.broadcast_text("Server", &room, &rest);
                }
                json!({"type":"notice","level":"info","text":"broadcast sent to all rooms"})
            }
        }
        "" => json!({"type":"error","text":"empty command"}),
        other => json!({"type":"error","text":format!("unknown command: {other} (try /help)")}),
    };
    let _ = socket.send(ws_text(reply.to_string())).await;
    Ok(())
}
