//! 密码哈希（argon2id）与会话令牌生成。

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::rngs::SysRng;
use rand::TryRng;

/// 使用 argon2id 对密码进行哈希，并返回 PHC 编码的字符串。使用
/// [`verify_password`] 对照其检查明文密码。
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let mut salt_bytes = [0u8; 16];
    // 自行生成随机盐：argon2 捆绑的 `password-hash` 在编译时没有启用
    // `rand_core/getrandom` 特性，而 rand 0.10 的 `SysRng` 针对的是不同代际的
    // `rand_core`，因此我们无法直接把 RNG 交给 `SaltString::generate`。
    // 将 16 个随机字节编码为 b64 盐与直接使用效果等价，
    // 并且避免了任何特性上的迂回。
    SysRng
        .try_fill_bytes(&mut salt_bytes)
        .expect("OS random source unavailable");
    let salt = SaltString::encode_b64(&salt_bytes)?;
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string();
    Ok(hash)
}

/// 校验明文密码与存储的 PHC 编码哈希是否匹配。
pub fn verify_password(password: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// 生成 32 字节的随机会话令牌，以类 base64url 的十六进制编码。
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("OS random source unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
