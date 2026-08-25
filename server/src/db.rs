//! SQLite 持久化层（通过 `rusqlite` 内嵌 SQLite）。

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

/// 一个已持久化的用户记录。
#[derive(Debug, Clone)]
pub struct UserRecord {
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub banned: i64,
}

impl UserRecord {
    /// 该用户是否被封禁。
    pub fn is_banned(&self) -> bool {
        self.banned != 0
    }
}

/// 一个线程安全的 SQLite 封装。`rusqlite::Connection` 是 `!Sync`，因此所有
/// 访问都通过一个 `Mutex` 串行化。对于参考服务器来说足够了。
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// 打开（如有需要则创建）位于 `path` 的数据库。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create db directory")?;
        }
        let conn = Connection::open(path).context("open sqlite database")?;
        let db = Db {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                username      TEXT PRIMARY KEY NOT NULL,
                password_hash TEXT NOT NULL,
                role          TEXT NOT NULL,
                banned        INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS rooms (
                room TEXT PRIMARY KEY NOT NULL
            );",
        )
        .context("migrate schema")?;
        Ok(())
    }

    /// 插入一个新用户。
    pub fn create_user(&self, username: &str, password_hash: &str, role: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash, role, banned) VALUES (?1, ?2, ?3, 0)",
            params![username, password_hash, role],
        )
        .context("insert user")?;
        Ok(())
    }

    /// 按用户名查找用户。
    pub fn get_user(&self, username: &str) -> Result<Option<UserRecord>> {
        let conn = self.conn.lock().unwrap();
        let rec = conn
            .query_row(
                "SELECT username, password_hash, role, banned FROM users WHERE username = ?1",
                params![username],
                |row| {
                    Ok(UserRecord {
                        username: row.get(0)?,
                        password_hash: row.get(1)?,
                        role: row.get(2)?,
                        banned: row.get(3)?,
                    })
                },
            )
            .optional()
            .context("query user")?;
        Ok(rec)
    }

    /// 设置用户的角色。
    pub fn set_role(&self, username: &str, role: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET role = ?1 WHERE username = ?2",
            params![role, username],
        )
        .context("set role")?;
        Ok(())
    }

    /// 设置用户的密码哈希。
    pub fn set_password(&self, username: &str, password_hash: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET password_hash = ?1 WHERE username = ?2",
            params![password_hash, username],
        )
        .context("set password")?;
        Ok(())
    }

    /// 设置封禁标志。
    pub fn set_banned(&self, username: &str, banned: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET banned = ?1 WHERE username = ?2",
            params![banned as i64, username],
        )
        .context("set banned")?;
        Ok(())
    }

    /// 删除一个用户。
    pub fn delete_user(&self, username: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM users WHERE username = ?1", params![username])
            .context("delete user")?;
        Ok(())
    }

    /// 列出已持久化的房间名。
    pub fn list_rooms(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT room FROM rooms").context("prepare list rooms")?;
        let rooms = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("query rooms")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect rooms")?;
        Ok(rooms)
    }

    /// 持久化一个房间。
    pub fn create_room(&self, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("INSERT OR IGNORE INTO rooms (room) VALUES (?1)", params![name])
            .context("insert room")?;
        Ok(())
    }

    /// 移除一个已持久化的房间。
    pub fn delete_room(&self, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM rooms WHERE room = ?1", params![name])
            .context("delete room")?;
        Ok(())
    }

    /// 如果尚不存在管理员，则创建一个 `admin` 账户（用户名为 `admin`）。
    /// 如果创建了管理员则返回 `true`。
    pub fn ensure_admin(&self, password: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let exists: Option<String> = conn
            .query_row(
                "SELECT username FROM users WHERE role = 'admin' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("query admin")?;
        drop(conn);
        if exists.is_some() {
            return Ok(false);
        }
        let hash = crate::auth::hash_password(password)
            .map_err(|e| anyhow::anyhow!("hash admin password: {e}"))?;
        self.create_user("admin", &hash, "admin")?;
        Ok(true)
    }
}
