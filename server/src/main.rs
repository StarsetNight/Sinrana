use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use sinrana_server::Config;

#[derive(Parser)]
#[command(name = "sinrana-server", version, about = "Sinrana reference QUIC chat server")]
struct Args {
    /// 可选 TOML 配置文件的路径。通常使用默认值，否则回退到 `server/config.toml`。
    /// 此处的值会被下面的 CLI 标志覆盖。
    #[arg(long)]
    config: Option<PathBuf>,
    /// UDP/QUIC 绑定地址（覆盖配置文件）。
    #[arg(long)]
    bind: Option<String>,
    /// 每个用户加入的默认房间（覆盖配置文件）。
    #[arg(long)]
    default_room: Option<String>,
    /// SQLite 数据库路径（覆盖配置文件）。
    #[arg(long)]
    db: Option<PathBuf>,
    /// TLS 证书链（DER）。如果省略，则会生成一个自签名证书。
    #[arg(long)]
    cert: Option<PathBuf>,
    /// TLS 私钥（DER）。与 `--cert` 配对。
    #[arg(long)]
    key: Option<PathBuf>,
    /// 允许新用户注册（覆盖配置文件）。
    #[arg(long)]
    allow_register: Option<bool>,
    /// 需要已认证的账户（不允许访客登录）。
    #[arg(long)]
    force_account: Option<bool>,
    /// 锁定服务器，只允许管理员登录。
    #[arg(long)]
    lock_server: bool,
    /// 持久化聊天记录。
    #[arg(long)]
    record: bool,
    /// 启用管理 WebUI。
    #[arg(long)]
    web: Option<bool>,
    /// 管理 WebUI 的 TCP 地址。
    #[arg(long)]
    web_addr: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let args = Args::parse();

    // 解析配置文件路径：显式指定 `--config`，否则使用默认值。
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from("server/config.toml"));
    let mut cfg = Config::load(Some(&config_path))?;

    // 在（文件 + 默认值）配置之上应用 CLI 覆盖。
    let addr_resolved = args
        .bind
        .as_deref()
        .map(|s| s.parse::<SocketAddr>())
        .transpose()
        .context("invalid --bind address")?;
    if let Some(addr) = addr_resolved {
        cfg.bind_addr = addr;
    }
    if let Some(room) = args.default_room {
        cfg.default_room = room;
    }
    if let Some(db) = args.db {
        cfg.db_path = db;
    }
    if args.cert.is_some() {
        cfg.cert_file = args.cert;
    }
    if args.key.is_some() {
        cfg.key_file = args.key;
    }
    if let Some(v) = args.allow_register {
        cfg.allow_register = v;
    }
    if let Some(v) = args.force_account {
        cfg.force_account = v;
    }
    if args.lock_server {
        cfg.lock_server = true;
    }
    if args.record {
        cfg.record_history = true;
    }
    if let Some(web) = args.web {
        cfg.web_enabled = web;
    }
    if let Some(s) = &args.web_addr {
        cfg.web_addr = s.parse().context("invalid --web-addr address")?;
    }

    let data_dir = cfg
        .db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    cfg.data_dir = data_dir;

    let server = sinrana_server::Server::start_with_config(cfg, Some(config_path)).await?;
    println!(
        "Sinrana server {} listening on {} (default admin pass: admin/admin)",
        server.server_version(),
        server.local_addr()?,
    );
    if server.web_enabled() {
        println!("WebUI listening on http://{}", server.web_addr());
    } else {
        println!("WebUI disabled");
    }
    server.run().await
}
