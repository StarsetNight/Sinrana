//! 管理 WebUI HTTP 接口的集成测试：提供索引页面、
//! 拒绝未认证的 API 调用，并在提供实时监控数据前接受管理员登录。
//! 运行一个真实的服务器，并在独立的临时端口上启用 WebUI。
//! 使用一个基于无 TLS TCP 的最小化原始 HTTP/1.1 客户端，因此
//! 无需额外依赖。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sinrana_server::{Config, Server};

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_db() -> std::path::PathBuf {
    let n = DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("sinrana-web-{}-{}.sqlite", std::process::id(), n));
    let _ = std::fs::remove_file(&p);
    p
}

async fn find_free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

async fn start_server() -> (SocketAddr, SocketAddr) {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        web_addr: format!("127.0.0.1:{}", find_free_port().await).parse().unwrap(),
        db_path: tmp_db(),
        default_room: "Sinrana! Chatting Room".into(),
        web_enabled: true,
        ..Default::default()
    };
    let server = Server::start(cfg).await.unwrap();
    let addr = server.local_addr().unwrap();
    let web = server.web_addr();
    tokio::spawn(async move { let _ = server.run().await; });
    (addr, web)
}

async fn wait_web_ready(web: SocketAddr) {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(web).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 发送原始 HTTP/1.1 请求并读取响应；`head` 为请求行加请求头行，
/// `body` 为可选的请求体。
/// 返回 `(status_line, body)`。
async fn http_raw(web: SocketAddr, head: &str, body: &str) -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(web).await.unwrap();
    let mut req = head.to_string();
    req.push_str("\r\nConnection: close");
    req.push_str("\r\n\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = match text.split_once("\r\n\r\n") {
        Some((h, b)) => (h.to_string(), b.to_string()),
        None => (text.clone(), String::new()),
    };
    let status_line = head.lines().next().unwrap_or_default().to_string();
    (status_line, body)
}

#[tokio::test]
async fn web_ui_serves_index_and_rejects_anonymous() {
    let (_addr, web) = start_server().await;
    wait_web_ready(web).await;

    // 无需认证即可访问索引页面。
    let (status, body) = http_raw(web, "GET / HTTP/1.1", "").await;
    assert!(status.starts_with("HTTP/1.1 200"), "expected 200, got {status}");
    assert!(body.contains("Sinrana"), "index should reference Sinrana");

    // 未认证的 /api/info 会被拒绝。
    let (status, body) = http_raw(web, "GET /api/info HTTP/1.1", "").await;
    assert!(status.starts_with("HTTP/1.1 401"), "expected 401, got {status}");
    assert!(body.contains("unauthorized"), "body: {body}");
}

#[tokio::test]
async fn web_ui_admin_login_returns_token() {
    let (_addr, web) = start_server().await;
    wait_web_ready(web).await;

    // 错误的密码必须被拒绝。
    let bad_body = r#"{"username":"admin","password":"wrong"}"#;
    let bad_head = format!(
        "POST /api/login HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: x",
        bad_body.len()
    );
    let (status, _) = http_raw(web, &bad_head, bad_body).await;
    assert!(status.starts_with("HTTP/1.1 401"), "expected 401, got {status}");

    // 正确的管理员凭据会返回令牌。
    let body = r#"{"username":"admin","password":"admin"}"#;
    let head = format!(
        "POST /api/login HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: x",
        body.len()
    );
    let (status, body) = http_raw(web, &head, body).await;
    assert!(status.starts_with("HTTP/1.1 200"), "login failed: {status} {body}");
    let v: serde_json::Value = serde_json::from_str(&body).expect("login json");
    let token = v["token"].as_str().expect("token present").to_string();

    // 已认证的 /api/info 返回实时数据。
    let head = format!("GET /api/info HTTP/1.1\r\nAuthorization: Bearer {token}");
    let (status, body) = http_raw(web, &head, "").await;
    assert!(status.starts_with("HTTP/1.1 200"), "info failed: {status}");
    let info: serde_json::Value = serde_json::from_str(&body).expect("info json");
    assert_eq!(info["name"], "Sinrana");
    assert!(info["online"].as_u64().is_some());
    assert!(info["rooms"].as_u64().unwrap() >= 1);
}
