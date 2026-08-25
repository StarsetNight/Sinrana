use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use sinrana_cli::Client;
use sinrana_proto::Message;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(name = "sinrana-cli", version, about = "Sinrana CLI chat client")]
struct Args {
    /// 服务器 UDP/QUIC 地址，例如 127.0.0.1:8080。
    #[arg(long)]
    server: String,
    /// TLS 服务器名称（必须与证书的 SAN 匹配）。
    #[arg(long, default_value = "localhost")]
    name: String,
    /// 要信任的服务器证书（DER）路径。
    #[arg(long)]
    cert: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let addr = args.server.parse().context("invalid server address")?;
    let cert_der = std::fs::read(&args.cert).context("read certificate")?;

    let mut client = Client::connect(addr, &args.name, &cert_der).await?;
    let (username, password) = prompt_login().await?;

    match client.request(Message::Login { username, password }).await? {
        Message::LoginRes { token, username, roles, .. } => {
            println!("Logged in as {username} (roles: {roles:?})");
            println!("To reconnect, resume token: {token}");
            // 排空登录时的推送（房间列表、清单，……）。
            drain(&mut client).await;
        }
        other => {
            print_message(&other);
            return Ok(());
        }
    }

    repl(&mut client).await
}

async fn prompt_login() -> anyhow::Result<(String, String)> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    println!("Username: ");
    let mut name = String::new();
    stdin.read_line(&mut name).await?;
    println!("Password: ");
    let mut pass = String::new();
    stdin.read_line(&mut pass).await?;
    Ok((name.trim().to_string(), pass.trim().to_string()))
}

async fn repl(client: &mut Client) -> anyhow::Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    println!("Enter commands. Type /help for help, /quit to exit.");
    loop {
        let mut line = String::new();
        let n = stdin.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        handle(client, &line).await?;
        drain(client).await;
    }
    Ok(())
}

async fn handle(client: &mut Client, line: &str) -> anyhow::Result<()> {
    let (cmd, rest) = line
        .split_once(' ')
        .map(|(a, b)| (a, b.trim().to_string()))
        .unwrap_or((line, String::new()));

    let resp = match cmd {
        "/help" => {
            println!("Commands:");
            println!("  /register <user> <pass>");
            println!("  /create <room>   /delete <room>   /join <room>   /leave <room>");
            println!("  /rooms           /members <room>");
            println!("  /send <room> <text>   /dm <user> <text>");
            println!("  /history <room> [n]   /users   /managers   /info   /quit");
            return Ok(());
        }
        "/quit" => return Ok(()),
        "/register" => {
            let mut it = rest.split_whitespace();
            let (Some(u), Some(p)) = (it.next(), it.next()) else {
                println!("usage: /register <user> <pass>");
                return Ok(());
            };
            client.request(Message::Register { username: u.into(), password: p.into() }).await?
        }
        "/create" => {
            client.request(Message::RoomCreate { name: rest }).await?
        }
        "/delete" => client.request(Message::RoomDelete { name: rest }).await?,
        "/join" => client.request(Message::RoomJoin { name: rest }).await?,
        "/leave" => client.request(Message::RoomLeave { name: rest }).await?,
        "/rooms" => client.request(Message::RoomList).await?,
        "/members" => client.request(Message::RoomMembers { name: rest }).await?,
        "/history" => {
            let mut it = rest.split_whitespace();
            let room = it.next().unwrap_or_default().to_string();
            let limit = it.next().and_then(|x| x.parse().ok()).unwrap_or(50);
            client.request(Message::History { room, limit, before_seq: None }).await?
        }
        "/users" => client.request(Message::UserOnline).await?,
        "/managers" => client.request(Message::UserManagers).await?,
        "/info" => client.request(Message::ServerInfo).await?,
        "/send" => {
            let mut it = rest.splitn(2, ' ');
            let (Some(room), Some(text)) = (it.next(), it.next()) else {
                println!("usage: /send <room> <text>");
                return Ok(());
            };
            client.request(Message::SendText { room: Some(room.into()), to: None, body: text.into() }).await?
        }
        "/dm" => {
            let mut it = rest.splitn(2, ' ');
            let (Some(to), Some(text)) = (it.next(), it.next()) else {
                println!("usage: /dm <user> <text>");
                return Ok(());
            };
            client.request(Message::SendText { room: None, to: Some(to.into()), body: text.into() }).await?
        }
        "" => return Ok(()),
        other => {
            println!("unknown command: {other}");
            return Ok(());
        }
    };
    print_message(&resp);
    Ok(())
}

/// 打印已缓冲的推送以及任何立即可用的入站帧。
async fn drain(client: &mut Client) {
    for _ in 0..32 {
        match tokio::time::timeout(Duration::from_millis(20), client.recv_push()).await {
            Ok(Ok(frame)) => print_message(&frame.msg),
            _ => break,
        }
    }
}

fn print_message(msg: &Message) {
    match msg {
        Message::RegisterRes { username } => println!("Registered: {username}"),
        Message::RoomListRes { rooms, mine } => println!("Rooms: {rooms:?}\nMine: {mine:?}"),
        Message::RoomCreateRes { name } => println!("Created room: {name}"),
        Message::RoomJoinRes { name } => println!("Joined: {name}"),
        Message::RoomLeaveRes { name } => println!("Left: {name}"),
        Message::RoomDeleteRes { name } => println!("Deleted room: {name}"),
        Message::RoomMembersRes { name, members } => println!("{name} members: {members:?}"),
        Message::HistoryRes { room, messages } => {
            println!("History in {room}:");
            for m in messages {
                println!("  [{}] {}: {}", m.ts, m.from, m.body);
            }
        }
        Message::UserOnlineRes { users } => println!("Online: {users:?}"),
        Message::UserManagersRes { managers } => println!("Managers: {managers:?}"),
        Message::ServerInfoRes { name, version, rooms, online, protocol } => {
            println!("{name} v{version} (protocol {protocol}): {rooms} rooms, {online} online")
        }
        Message::SendTextRes { seq, ts } => println!("Sent (seq {seq}, ts {ts})"),
        Message::PushText { room, from, body, ts, .. } => println!("[{room}] {from}: {body} ({ts})"),
        Message::PushPrivate { from, body, .. } => println!("[DM] {from}: {body}"),
        Message::PushUserManifest { users } => println!("[manifest] online: {users:?}"),
        Message::Notice { text, level } => println!("[{level}] {text}"),
        Message::Error { code, message, .. } => eprintln!("[error {:?}] {message}", code),
        Message::Pong => println!("pong"),
        other => println!("{other:?}"),
    }
}
