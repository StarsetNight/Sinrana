//! 端到端集成测试：启动一个真实的 QUIC 服务器，并用实际客户端驱动它，
//! 覆盖注册/登录/房间/消息转发/历史/管理员等功能。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sinrana_cli::Client;
use sinrana_proto::Message;
use sinrana_server::{Config, Server};

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 每个测试唯一的 SQLite 路径（并行的各个 `#[tokio::test]` 各用一份）。
fn tmp_db() -> std::path::PathBuf {
    let n = DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("sinrana-e2e-{}-{}.sqlite", std::process::id(), n));
    let _ = std::fs::remove_file(&p);
    p
}

/// 在临时端口上启动服务器，启动其接受循环，并返回
/// 实际地址以及服务器证书的 DER。
async fn start_server() -> (std::net::SocketAddr, Vec<u8>) {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        db_path: tmp_db(),
        default_room: "Sinrana! Chatting Room".into(),
        // 这些测试只覆盖 QUIC 协议；禁用 HTTP WebUI，
        // 这样三个并行测试就不会争用默认的 Web 端口。
        web_enabled: false,
        ..Default::default()
    };
    let server = Server::start(cfg).await.unwrap();
    let addr = server.local_addr().unwrap();
    let cert = server.cert_der().to_vec();
    tokio::spawn(async move { let _ = server.run().await; });
    (addr, cert)
}

/// 注册用户并登录，返回会话客户端。
async fn register_and_login(addr: std::net::SocketAddr, cert: &[u8], username: &str, password: &str) -> Client {
    let mut c = Client::connect(addr, "localhost", cert).await.unwrap();
    let resp = c.request(Message::Register { username: username.into(), password: password.into() }).await.unwrap();
    assert!(matches!(resp, Message::RegisterRes { .. }), "register failed: {resp:?}");
    let resp = c.request(Message::Login { username: username.into(), password: password.into() }).await.unwrap();
    assert!(matches!(resp, Message::LoginRes { .. }), "login failed: {resp:?}");
    c
}

/// 仅登录（不注册）——用于预置的管理员账号。
async fn login_only(addr: std::net::SocketAddr, cert: &[u8], username: &str, password: &str) -> Client {
    let mut c = Client::connect(addr, "localhost", cert).await.unwrap();
    let resp = c.request(Message::Login { username: username.into(), password: password.into() }).await.unwrap();
    assert!(matches!(resp, Message::LoginRes { .. }), "login failed: {resp:?}");
    c
}

/// 持续读取推送，直到看到 `chat.private` 推送。返回 (from, body)。
async fn recv_private(client: &mut Client) -> Option<(String, String)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let frame = match tokio::time::timeout_at(deadline, client.recv_push()).await {
            Ok(Ok(f)) => f,
            _ => return None,
        };
        if let Message::PushPrivate { from, body, .. } = frame.msg {
            return Some((from, body));
        }
    }
}

/// 持续读取推送，直到看到针对 `room` 的 `chat.text` 推送，忽略其他推送。
async fn recv_room_text(client: &mut Client, room: &str) -> Option<(String, String)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let frame = match tokio::time::timeout_at(deadline, client.recv_push()).await {
            Ok(Ok(f)) => f,
            _ => return None,
        };
        if let Message::PushText { room: r, from, body, .. } = frame.msg {
            if &r == room {
                return Some((from, body));
            }
        }
    }
}

#[tokio::test]
async fn register_login_broadcast_and_history() {
    let (addr, cert) = start_server().await;
    let mut alice = register_and_login(addr, &cert, "alice", "password1").await;
    let mut bob = register_and_login(addr, &cert, "bob", "password2").await;
    let room = "Sinrana! Chatting Room";

    let resp = alice.request(Message::SendText { room: Some(room.into()), to: None, body: "hello bob".into() }).await.unwrap();
    assert!(matches!(resp, Message::SendTextRes { .. }), "send failed: {resp:?}");

    let (from, body) = recv_room_text(&mut bob, room).await.expect("bob should receive");
    assert_eq!(from, "alice");
    assert_eq!(body, "hello bob");

    let (from_alice, _) = recv_room_text(&mut alice, room).await.expect("alice echo");
    assert_eq!(from_alice, "alice");

    match alice.request(Message::History { room: room.into(), limit: 50, before_seq: None }).await.unwrap() {
        Message::HistoryRes { messages, .. } => {
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].body, "hello bob");
        }
        other => panic!("unexpected history: {other:?}"),
    }
}

#[tokio::test]
async fn private_message_and_online_list() {
    let (addr, cert) = start_server().await;
    let mut alice = register_and_login(addr, &cert, "carol", "password1").await;
    let mut bob = register_and_login(addr, &cert, "dave", "password2").await;

    match alice.request(Message::UserOnline).await.unwrap() {
        Message::UserOnlineRes { users } => {
            assert!(users.contains(&"carol".to_string()));
            assert!(users.contains(&"dave".to_string()));
        }
        other => panic!("unexpected: {other:?}"),
    }

    let _ = alice.request(Message::SendText { room: None, to: Some("dave".into()), body: "psst".into() }).await.unwrap();
    let (from, body) = recv_private(&mut bob).await.expect("dave should receive private");
    assert_eq!(from, "carol");
    assert_eq!(body, "psst");
}

#[tokio::test]
async fn admin_can_create_room_but_user_cannot() {
    let (addr, cert) = start_server().await;
    let mut user = register_and_login(addr, &cert, "eve", "password1").await;
    match user.request(Message::RoomCreate { name: "secret".into() }).await.unwrap() {
        Message::Error { code: sinrana_proto::ErrorCode::Forbidden, .. } => {}
        other => panic!("expected forbidden, got {other:?}"),
    }

    let mut admin = login_only(addr, &cert, "admin", "admin").await;
    match admin.request(Message::RoomCreate { name: "secret".into() }).await.unwrap() {
        Message::RoomCreateRes { name } => assert_eq!(name, "secret"),
        other => panic!("create failed: {other:?}"),
    }
}