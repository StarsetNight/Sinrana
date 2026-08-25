//! 服务器配置：默认值 + TOML 配置文件 + CLI 覆盖。
//!
//! 优先级为 `defaults < 配置文件 < CLI 标志 < 在线编辑`（Web 配置保存会写入
//! 文件，并热应用其中运行时安全的子集）。
//!
//! 配置文件是**可选的**。当存在时，它是一个 TOML 文档，其键与字段名匹配
//! (snake_case)；任何被省略的键都会回退到 [`Config::default`] 的值，因为该
//! 结构体标注了 `#[serde(default)]`。

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// 运行时服务器配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// UDP/QUIC 监听地址。
    pub bind_addr: SocketAddr,
    /// 每个用户登录时加入的默认房间。
    pub default_room: String,
    /// SQLite 数据库文件的路径。
    pub db_path: PathBuf,
    /// 运行时文件（证书等）的目录。
    pub data_dir: PathBuf,
    /// 允许新用户注册。
    pub allow_register: bool,
    /// 需要已认证的账户（不允许访客登录）。
    pub force_account: bool,
    /// 锁定服务器，只允许管理员登录。
    pub lock_server: bool,
    /// 将聊天记录持久化到 SQLite（除内存之外）。
    pub record_history: bool,
    /// 可选的证书链（DER）。如果为空，则会生成一个自签名证书，
    /// 用于开发环境。若此路径已设置但文件不存在，服务启动时会就地生成
    /// 并写入该路径（`key_file` 亦然），后续启动复用同一证书。
    pub cert_file: Option<PathBuf>,
    /// 可选的私钥（DER），与 `cert_file` 配对。
    pub key_file: Option<PathBuf>,
    /// 是否提供管理 WebUI。
    pub web_enabled: bool,
    /// 管理 WebUI（HTTP + WebSocket）的 TCP 地址。
    pub web_addr: SocketAddr,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8080".parse().expect("valid default addr"),
            default_room: "Sinrana! Chatting Room".into(),
            db_path: "server/data/sinrana.sqlite".into(),
            data_dir: "server/data".into(),
            allow_register: true,
            force_account: true,
            lock_server: false,
            record_history: false,
            cert_file: None,
            key_file: None,
            web_enabled: true,
            web_addr: "127.0.0.1:8090".parse().expect("valid default web addr"),
        }
    }
}

impl Config {
    /// 从可选的 TOML 文件加载配置。对于缺失的键，缺失/格式错误的配置会回退到
    /// 默认值；语法无效的文件会报错，这样错误的编辑会被暴露出来，
    /// 而不是被静默忽略。
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let mut cfg = Self::default();
        if let Some(p) = path {
            if p.exists() {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| anyhow::anyhow!("read config {}: {e}", p.display()))?;
                let file_cfg: Config = toml::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("parse config {}: {e}", p.display()))?;
                // serde 会用默认值填充被省略的字段，因此合并时采用显式提供的值；
                // 最简单且正确的做法是从文件重新推导出完整的 Config
                // (serde(default))，对于不存在的键它已经会给出默认值，
                // 无需再手动补全。
                cfg = file_cfg;
            }
        }
        Ok(cfg)
    }

    /// 将此配置以 TOML 格式持久化到 `path`（原子操作：在同一目录写入临时文件，
    /// 然后再重命名覆盖目标文件）。
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow::anyhow!("create config dir {}: {e}", parent.display()))?;
            }
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("serialize config: {e}"))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .map_err(|e| anyhow::anyhow!("write config {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| anyhow::anyhow!("rename config over {}: {e}", path.display()))?;
        Ok(())
    }

    /// 当收到 Web 配置保存时可实时应用、无需重启的字段。其他所有字段
    /// (`bind_addr`, `db_path`, `data_dir`, `cert_file`, `key_file`,
    /// `web_addr`) 都需要重启。
    pub fn runtime_fields(&self) -> Vec<&'static str> {
        vec![
            "default_room",
            "allow_register",
            "force_account",
            "lock_server",
            "record_history",
            "web_enabled",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn tmp_file(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("sinrana-cfg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert!(cfg.allow_register);
        assert!(cfg.force_account);
        assert!(cfg.web_enabled);
        assert!(!cfg.lock_server);
        assert!(!cfg.record_history);
        assert_eq!(cfg.web_addr.to_string(), "127.0.0.1:8090");
    }

    #[test]
    fn load_missing_file_yields_defaults() {
        let cfg = Config::load(Some(std::path::Path::new("no/such/config.toml"))).unwrap();
        let d = Config::default();
        assert_eq!(cfg.default_room, d.default_room);
        assert_eq!(cfg.web_addr, d.web_addr);
    }

    #[test]
    fn save_and_load_round_trips() {
        let p = tmp_file("roundtrip.toml");
        let mut cfg = Config::default();
        cfg.default_room = "Lounge".into();
        cfg.lock_server = true;
        cfg.web_enabled = false;
        cfg.bind_addr = "127.0.0.1:9191".parse().unwrap();
        cfg.save(&p).unwrap();

        let loaded = Config::load(Some(&p)).unwrap();
        assert_eq!(loaded.default_room, "Lounge");
        assert!(loaded.lock_server);
        assert!(!loaded.web_enabled);
        assert_eq!(loaded.bind_addr.to_string(), "127.0.0.1:9191");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn file_missing_keys_fall_back_to_defaults() {
        let p = tmp_file("partial.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        // 只指定了一个键；其余必须来自 Default。
        f.write_all(b"lock_server = true\n").unwrap();
        drop(f);
        let cfg = Config::load(Some(&p)).unwrap();
        assert!(cfg.lock_server);
        assert_eq!(cfg.web_addr, Config::default().web_addr);
        assert_eq!(cfg.default_room, Config::default().default_room);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let p = tmp_file("bad.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"this is not toml = [unclosed\n").unwrap();
        drop(f);
        assert!(Config::load(Some(&p)).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn runtime_fields_classification() {
        let cfg = Config::default();
        let rt = cfg.runtime_fields();
        assert!(rt.contains(&"allow_register"));
        assert!(rt.contains(&"default_room"));
        // 仅重启的字段不会出现在运行时列表中。
        assert!(!rt.contains(&"bind_addr"));
        assert!(!rt.contains(&"web_addr"));
    }
}
