# Sinrana 通信协议 — v1.0

**状态：参考实现（Rust）。**
本文档是 Sinrana 开放聊天协议的权威、与语言无关的规格说明。位于 `proto/` 的 Rust 参考编解码器是唯一的事实来源，必须与本文档保持同步。

---

## 1. 概述

Sinrana 是一个客户端/服务器聊天协议。**服务器**在命名的**房间**中的**客户端**之间转发消息，支持**私聊（1:1）**消息，并可选择地管理带有三级**角色**模型的**用户账户**。

该协议刻意保持极简，并且与传输方式无关：

- **传输：** QUIC（UDP），内置 TLS 1.3。后续可针对相同的帧协议添加纯 TCP 传输。
- **序列化：** MessagePack，由 `serde` + `rmp-serde` 生成。下列每个字段名都是稳定的，并在此处加以说明。
- **分帧：** 长度前缀 `u32` 大端序 + payload。

除非另有说明，所有多字节整数在线上均为**大端序**。

---

## 2. 传输与分帧

### 2.1 连接

- 服务器监听单个 UDP/QUIC 端点。
- 每个客户端在连接时打开**一条**双向 QUIC 流。该会话的所有帧都在此流上双向流动。
- 由于 QUIC 流是可靠且有序的，流内的消息顺序有保证，协议无需字节分隔符分帧（旧的 `\0` 分帧及其"粘包"规避方案已移除）。

### 2.2 帧

每个帧为：

```
+--------+------------------+
|  u32   |  payload (bytes) |
| length |                  |
+--------+------------------+
```

- `length` 为 `payload` 的字节长度，大端序。
- `payload` 是 [`Envelope`](#3-envelope) 的 **MessagePack** 编码。
- 允许的最大 `length` 为 **16 MiB**；更大的帧属于协议错误。

---

## 3. 信封（Envelope）

每个帧的 payload 是一个 MessagePack map `Envelope`：

| 字段 | 类型 | 描述 |
|-------|-------|-------------|
| `ver` | `u16` | 协议版本。本规格为 `1`。对端拒绝不匹配。 |
| `id`  | `u64` | 关联 id。`0` 表示*服务器推送*（无请求）。 |
| `msg` | `Message` | 消息（第 5 节）。 |

编码：对 `Message` 施加 `#[serde(tag = "t")]` —— 即 `result["t"]` 是判别符字符串（例如 `"chat.send"`）；其余字段构成 payload。因此 `msg` 对象包含一个 `t` 键以及该变体的字段。

### 3.1 关联

- 一个**请求**具有非零的 `id`。服务器用一个响应（或 `sys.error`）回复，并携带**相同**的 `id`。
- 一个**服务器推送**使用 `id == 0`，不与任何请求关联。

---

## 4. 认证与安全

- **密码**在服务器端使用 **argon2id**（PHC 字符串）散列。它们从不以明文存储或传输。
- **TLS：** QUIC 为整个连接提供 TLS 1.3。密码和令牌在加密信道内传输。
- **会话：** 登录时服务器签发一个**32 字节令牌**（十六进制）。客户端可在重连后使用该令牌 `auth.resume`。
- **权限：** 三个角色，顺序为 `user < manager < admin`。

---

## 5. 消息集

`type Message = { t: string, ... }`。命名空间：`auth`、`chat`、`room`、`user`、`admin`、`sys`。

### 5.1 `auth`

| `t` | 请求/响应 | 字段 |
|-----|------------------|--------|
| `auth.login` | 请求 | `username: string`、`password: string` |
| `auth.login_res` | 响应 | `user_id: u64`、`username: string`、`roles: string[]`、`token: string` |
| `auth.register` | 请求 | `username: string`、`password: string` |
| `auth.register_res` | 响应 | `username: string` |
| `auth.resume` | 请求 | `token: string` |
| `auth.resume_res` | 响应 | `username: string`、`roles: string[]` |
| `auth.logout` | 请求 | — |

### 5.2 `chat`

| `t` | 类型 | 字段 |
|-----|------|--------|
| `chat.send` | 请求 | `room: string?`、`to: string?`、`body: string` |
| `chat.send_res` | 响应 | `seq: u64`、`ts: i64` |
| `chat.text` | 推送 | `room: string`、`from: string`、`body: string`、`seq: u64`、`ts: i64` |
| `chat.private` | 推送 | `from: string`、`body: string`、`ts: i64` |
| `chat.history` | 请求 | `room: string`、`limit: u32`、`before_seq: u64?` |
| `chat.history_res` | 响应 | `room: string`、`messages: HistoryItem[]` |

- `chat.send`：`room`（房间广播）与 `to`（私聊消息）中必须恰好设置一个。`body` 非空。
- `HistoryItem = { seq, room, from, body, ts }`。

### 5.3 `room`

| `t` | 类型 | 字段 |
|-----|------|--------|
| `room.list` | 请求 | — |
| `room.list_res` | 响应 | `rooms: string[]`（全部）、`mine: string[]` |
| `room.create` | 请求 | `name: string`（Manager 及以上） |
| `room.create_res` | 响应 | `name: string` |
| `room.join` | 请求 | `name: string` |
| `room.join_res` | 响应 | `name: string` |
| `room.leave` | 请求 | `name: string` |
| `room.leave_res` | 响应 | `name: string` |
| `room.delete` | 请求 | `name: string`（Admin） |
| `room.delete_res` | 响应 | `name: string` |
| `room.members` | 请求 | `name: string` |
| `room.members_res` | 响应 | `name: string`、`members: string[]` |

用户登录时自动成为**默认房间**的成员，且不能离开它。

### 5.4 `user & admin`

| `t` | 类型 | 字段 | 权限 |
|-----|------|--------|-----------|
| `user.online` | 请求 | — | 任意 |
| `user.online_res` | 响应 | `users: string[]` | 任意 |
| `user.manifest` | 推送 | `users: string[]` | 任意（加入/离开时广播） |
| `user.managers` | 请求 | — | 任意 |
| `user.managers_res` | 响应 | `managers: string[]` | 任意 |
| `user.kick` | 请求 | `target: string`、`reason: string?` | Manager 及以上 |
| `user.kick_res` | 响应 | `target: string` | Manager 及以上 |
| `admin.create` | 请求 | `username: string`、`password: string`、`role: Role` | Admin |
| `admin.create_res` | 响应 | `username: string`、`role: Role` | Admin |
| `admin.set_pwd` | 请求 | `target: string`、`password: string` | Admin |
| `admin.set_pwd_res` | 响应 | `target: string` | Admin |
| `admin.set_role` | 请求 | `target: string`、`role: Role` | Admin |
| `admin.set_role_res` | 响应 | `target: string`、`role: Role` | Admin |
| `admin.delete` | 请求 | `target: string` | Admin |
| `admin.delete_res` | 响应 | `target: string` | Admin |
| `admin.ban` | 请求 | `target: string`、`reason: string?` | Admin |
| `admin.ban_res` | 响应 | `target: string` | Admin |
| `admin.restore` | 请求 | `target: string` | Admin |
| `admin.restore_res` | 响应 | `target: string` | Admin |

`Role = "user" | "manager" | "admin"`。

### 5.5 `sys`

| `t` | 类型 | 字段 |
|-----|------|--------|
| `sys.ping` | 请求 | — |
| `sys.pong` | 响应 | — |
| `sys.server_info` | 请求 | — |
| `sys.server_info_res` | 响应 | `protocol: u16`、`name: string`、`version: string`、`rooms: usize`、`online: usize` |
| `sys.notice` | 推送 | `text: string`、`level: string` |
| `sys.error` | 响应/推送 | `code: ErrorCode`、`message: string`、`retryable: bool` |

---

## 6. 错误模型

`ErrorCode` 的取值（serde 的 `SCREAMING_SNAKE_CASE`）：

| code | 含义 |
|------|---------|
| `BAD_REQUEST` | 格式错误的请求 |
| `AUTH_FAILED` | 凭据错误 |
| `NOT_AUTHENTICATED` | 无会话/会话无效 |
| `FORBIDDEN` | 权限不足 |
| `NOT_FOUND` | 目标未找到 |
| `CONFLICT` | 已存在（用户/房间/在线） |
| `RATE_LIMITED` | 速率限制 / 背压 |
| `INTERNAL` | 服务器端故障 |
| `VERSION_MISMATCH` | 协议版本不匹配 |
| `PROTOCOL` | 分帧/信封校验失败 |
| `ROOM_NOT_FOUND` | 目标房间缺失 |
| `USER_EXISTS` | 注册重复 |
| `NOT_IN_ROOM` | 发送者不是房间成员 |
| `RESERVED_NAME` | 保留/禁止的用户名 |

---

## 7. 排序与投递

- **服务器**是消息顺序的唯一权威。每一条被接受的房间消息都会被分配一个递增的 `seq` 和一个服务器端 `ts`。
- 房间消息被广播给所有当前房间成员（**包括发送者**，即回显），这样客户端就能一致地显示自己的文本。
- 私聊消息只投递给接收者。发送者会收到一个 `chat.send_res` 确认。

---

## 8. 连接生命周期

1. 客户端打开双向流，然后发送 `auth.login`（或 `auth.register`，或带先前令牌的 `auth.resume`）。
2. 服务器用匹配的 `*_res`，以及 `user.manifest` 和 `room.list` 推送来回复。
3. 客户端发送请求；服务器回复并推送 `chat.text` / `chat.private` / `sys.notice`。
4. 断开连接时，服务器把用户从所有房间移除，并广播一份更新的 `user.manifest`。

保活：发送 `sys.ping`；回复 `sys.pong`。

---

## 9. 参考实现布局

- `proto/` —— 编解码器：分帧、信封、消息枚举、错误码，以及 [`Transport`](proto/src/transport.rs) trait（QUIC 实现在 `proto/src/quic.rs`）。
- `server/` —— 参考 QUIC 服务器（配置、argon2id 认证、会话、房间、转发、管理、SQLite）。
- `cli/` —— 参考 CLI 客户端（交互式 REPL + 一次性），共享 `proto`。
- `docs/BUILD.md` —— 构建与依赖说明。

**管理 WebUI**（`server/src/web.rs`，通过 HTTP/WebSocket 提供服务）**不是**线上协议的一部分。它是一个面向浏览器的管理界面，读取并修改与 QUIC 端点相同的进程内服务器状态：它以管理员账户登录，展示实时的服务器/在线/房间数据，暴露一个根终端（以 `Server` 广播 + 服务器管理命令），并可编辑服务器配置（热应用运行时可安全子集）。它通过 HTTP 和 WS 讲 JSON，从不通过 QUIC 讲 msgpack。

---

## 10. 路线图 / 非目标（v1）

- **文件传输**（旧服务器中的 `SEND_FILE`）不在 v1 范围内；计划引入将来的 `file` 命名空间。
- **TCP 传输回退**是在同一 [`Transport`](proto/src/transport.rs) trait 之后计划添加的（QUIC 运行在 UDP 之上，某些 NAT/防火墙会丢弃 UDP）。
- **消息历史持久化**当前为内存态；SQLite 持久化是可选的（`--record`）。
- **管理 WebUI** 安全性：默认在 `127.0.0.1:8090` 上使用纯 HTTP；通过 WSS 提供服务（借助 hyper-rustls 使用 QUIC 证书）是计划中的后续工作。只有管理员角色的账户才能打开它。
