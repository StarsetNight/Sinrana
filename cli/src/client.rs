//! 一个供 REPL 二进制程序与集成测试共用的小型客户端库。

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::pki_types::CertificateDer;
use sinrana_proto::{Frame, Message, QuicTransport, RecvHalf, SendHalf};

/// 一个已连接的 Sinrana 客户端，通过 QUIC 使用该协议通信。
pub struct Client {
    #[allow(dead_code)]
    endpoint: quinn::Endpoint,
    #[allow(dead_code)]
    connection: quinn::Connection,
    send_half: SendHalf,
    recv_half: RecvHalf,
    next_id: u64,
    pending: VecDeque<Frame>,
}

impl Client {
    /// 连接服务器，信任给定的自签名证书 DER。
    pub async fn connect(
        addr: SocketAddr,
        server_name: &str,
        cert_der: &[u8],
    ) -> Result<Self> {
        // 显式选择 ring 加密提供程序（参见 Server::start）。
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(cert_der.to_vec()))
            .context("add root certificate")?;
        let client_crypto = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
                .context("quic client config")?,
        ));
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(client_config);
        let connection = endpoint
            .connect(addr, server_name)
            .context("connect")?
            .await
            .context("handshake")?;
        let (send, recv) = connection.open_bi().await.context("open stream")?;
        let transport = QuicTransport::new(send, recv);
        let (send_half, recv_half) = transport.split();
        Ok(Client {
            endpoint,
            connection,
            send_half,
            recv_half,
            next_id: 1,
            pending: VecDeque::new(),
        })
    }

    /// 发送请求并等待匹配的响应。带有不同 id 的帧
    /// （即服务器推送）会被缓冲，并由
    /// [`Client::recv_push`] 返回。
    pub async fn request(&mut self, msg: Message) -> Result<Message> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_half.send(&Frame::request(id, msg)).await?;
        loop {
            let frame = self.recv_half.recv().await.context("recv frame")?;
            if frame.id == id {
                return Ok(frame.msg);
            }
            self.pending.push_back(frame);
        }
    }

    /// 接收下一个已缓冲的推送，或从线路上读取下一个帧。
    pub async fn recv_push(&mut self) -> Result<Frame> {
        if let Some(f) = self.pending.pop_front() {
            return Ok(f);
        }
        self.recv_half.recv().await.context("recv frame")
    }

    /// 如果 `msg` 是 `sys.error`，则返回其消息。
    pub fn error_text(msg: &Message) -> Option<&str> {
        match msg {
            Message::Error { message, .. } => Some(message.as_str()),
            _ => None,
        }
    }
}
