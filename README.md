# Sinrana

**Sinrana** 是一个基于 C/S 架构的开放**通信协议**（QUIC + MessagePack），配套一个 **Rust 参考实现**：标准服务端（`sinrana-server`）与 CLI 客户端（`sinrana-cli`），二者共享同一个协议 crate（`sinrana-proto`）。

## 目录结构

```
Sinrana/
├── Cargo.toml      # Rust workspace（proto / server / cli）
├── docs/
│   └── protocol.md # 开放协议规格（权威规范，与 proto 保持一致）
├── proto/          # 协议 crate：分帧、信封、强类型消息、错误码、Transport trait
├── server/         # 标准服务端：QUIC、argon2id 认证 + TLS + 会话、房间、admin、SQLite
└── cli/            # CLI 客户端：连接、登录、房间/消息、交互式 REPL
```

## 技术栈

- **语言/运行时**：Rust（workspace，异步 `tokio`）
- **传输**：QUIC（`quinn`），内置 TLS 1.3
- **序列化**：MessagePack（`serde` + `rmp-serde`），长度前缀分帧
- **认证**：argon2id 密码散列 + 会话 token（32 字节）
- **持久化**：SQLite（`rusqlite` bundled）
- **CLI**：`clap`

## 构建 / 运行

```bash
cargo build --workspace            # 构建全部
cargo test  --workspace            # 协议单测 + 端到端集成测试

# 服务端（默认 bind 127.0.0.1:8080，admin/admin）
cargo run -p sinrana-server

# CLI 客户端（连接服务端，需其自签证书）
cargo run -p sinrana-cli -- --server 127.0.0.1:8080 --name localhost --cert <cert.der>
```

> 服务端默认生成一个**自签证书**（仅开发用）。生产环境请用 `--cert / --key` 指定真实证书。
> 各命令用法见 `docs/protocol.md`。

## 协议要点

- 单条 QUIC 双向流承载全部帧；`[u32 BE 长度][msgpack 信封]`。
- 信封 `Frame{ver, id, msg}`；请求/响应用同一 `id` 关联，`id==0` 为服务端推送。
- 消息按命名空间组织：`auth`、`chat`、`room`、`user`、`admin`、`sys`。
- 角色 `user < manager < admin`；房间创建需 Manager+，删除需 Admin。
- 服务端是消息顺序的唯一权威（分配 `seq`/`ts`）；密码 argon2id + TLS，不再有明文/MD5。

详见 [`docs/protocol.md`](docs/protocol.md)。

## 授权 / 版权

- Copyright (c) 2026 **StarsetNight**
- 本项目采用 **MIT License**（见 `LICENSE`）。
