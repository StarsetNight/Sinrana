//! 用于开发/测试的自签名证书生成与加载。

use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// 为 localhost / 127.0.0.1 生成自签名证书。
///
/// 这仅用于开发和集成测试。生产服务器应通过 `--cert` / `--key`
/// 提供真实证书。
pub fn self_signed() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let ck = rcgen::generate_simple_self_signed(vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "[::1]".into(),
    ])?;
    let cert_der = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
    Ok((cert_der, key))
}

/// 生成一个自签名证书，返回 (证书 DER, PKCS#8 私钥 DER)。
///
/// 与 [`self_signed`] 不同，这里直接给出可供 TLS 使用并可写盘的原始字节。
pub fn self_signed_der() -> Result<(Vec<u8>, Vec<u8>)> {
    let ck = rcgen::generate_simple_self_signed(vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "[::1]".into(),
    ])?;
    let cert_der = ck.cert.der().to_vec();
    let key_der = ck.signing_key.serialize_der();
    Ok((cert_der, key_der))
}

/// 加载位于 `cert_file` / `key_file` 的证书与私钥（DER）。
///
/// 若这一对路径已由配置给出但文件不存在，则就地生成一份自签名证书并写入这两个
/// 文件，让后续启动可以复用同一证书；随后返回生成的证书与私钥。
pub fn load_or_generate(cert_file: &Path, key_file: &Path) -> Result<(Vec<u8>, PrivateKeyDer<'static>)> {
    match (std::fs::read(cert_file), std::fs::read(key_file)) {
        (Ok(cert), Ok(key)) => Ok((cert, pkcs8_key(key))),
        (Err(ce), Err(ke)) => {
            if ce.kind() == ErrorKind::NotFound || ke.kind() == ErrorKind::NotFound {
                generate_and_save(cert_file, key_file)
            } else {
                Err(anyhow::anyhow!(
                    "无法读取证书/密钥：cert={ce}；key={ke}"
                ))
            }
        }
        (Err(ce), Ok(_)) if ce.kind() == ErrorKind::NotFound => generate_and_save(cert_file, key_file),
        (Err(ce), Ok(_)) => Err(anyhow::anyhow!("读取证书 {} 失败：{ce}", cert_file.display())),
        (Ok(_), Err(ke)) if ke.kind() == ErrorKind::NotFound => generate_and_save(cert_file, key_file),
        (Ok(_), Err(ke)) => Err(anyhow::anyhow!("读取密钥 {} 失败：{ke}", key_file.display())),
    }
}

/// 生成自签名 DER 证书/密钥，写入 `cert_file` / `key_file`（必要时创建父目录），
/// 并返回可用的私钥。
fn generate_and_save(cert_file: &Path, key_file: &Path) -> Result<(Vec<u8>, PrivateKeyDer<'static>)> {
    let (cert_der, key_der) = self_signed_der()?;
    if let Some(parent) = cert_file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("创建证书目录 {}", parent.display()))?;
    }
    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("创建密钥目录 {}", parent.display()))?;
    }
    std::fs::write(cert_file, &cert_der).with_context(|| format!("写入证书 {}", cert_file.display()))?;
    std::fs::write(key_file, &key_der).with_context(|| format!("写入密钥 {}", key_file.display()))?;
    tracing::info!("生成自签名证书并写入 {} / {}", cert_file.display(), key_file.display());
    Ok((cert_der, pkcs8_key(key_der)))
}

/// 把 PKCS#8 私钥 DER 字节包装成 rustls 兼容的私钥类型。
fn pkcs8_key(key_der: Vec<u8>) -> PrivateKeyDer<'static> {
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_generate_creates_missing_then_reuses() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("cert-test-{}", std::process::id()));
        let cert = dir.join("cert.der");
        let key = dir.join("key.der");
        let _ = std::fs::remove_dir_all(&dir);

        // 文件不存在 → 生成并落盘。
        let (c1, _k1) = load_or_generate(&cert, &key).unwrap();
        assert!(!c1.is_empty());
        assert!(cert.exists(), "证书文件应已生成");
        assert!(key.exists(), "密钥文件应已生成");
        assert_eq!(std::fs::read(&cert).unwrap(), c1);

        // 第二次启动直接复用磁盘上的证书，不再重生成。
        let (c2, _k2) = load_or_generate(&cert, &key).unwrap();
        assert_eq!(c2, c1, "复用现有证书时字节应一致");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
