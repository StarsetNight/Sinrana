//! Sinrana 线上协议的长度前缀分帧。
//!
//! 线上每个帧是 `[u32 大端长度][msgpack 载荷]`。
//! 这取代了会导致“粘包”问题的遗留 `\0` 分隔分帧，
//! 以及掩盖该问题的 `time.sleep` 变通方案。长度
//! 前缀使边界变得无歧义，因此单个流可以承载任意
//! 数量的帧而无需猜测。

use std::io;

/// 可接受的帧载荷最大大小（16 MiB）。防止损坏或
/// 恶意的对端声明一个会耗尽内存的巨大长度。
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// 长度前缀自身的长度。
pub const LEN_PREFIX: usize = 4;

/// 将载荷编码成带长度前缀的帧字节串。
pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(LEN_PREFIX + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}   

/// 尝试从 `buf` 的前端提取一个完整的帧。
///
/// 如果还没有足够字节来确定一个完整帧，则返回 `Ok(None)`；
/// 如果有一个完整帧可用，则返回 `Ok(Some((consumed, payload)))`；
/// 如果声明的长度超过 [`MAX_FRAME`]，则返回 `Err`。
pub fn decode_frame(buf: &[u8]) -> io::Result<Option<(usize, &[u8])>> {
    if buf.len() < LEN_PREFIX {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds max {MAX_FRAME}"),
        ));
    }
    let total = LEN_PREFIX + len;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some((total, &buf[LEN_PREFIX..total])))
}

/// 用于字节流之上的增量帧解析器。
///
/// 用 [`FrameCodec::push`] 喂入原始字节，然后用 [`FrameCodec::try_pop`]
/// 取出完整帧。部分帧会被缓存，直到下一次 `push`
///（或对端关闭）将其补全。
#[derive(Debug, Default)]
pub struct FrameCodec {
    buf: Vec<u8>,
}

impl FrameCodec {
    /// 创建一个空的编解码器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 将来自流的原始字节追加到编解码器缓冲区。
    pub fn push(&mut self, chunk: &[u8]) -> io::Result<()> {
        self.buf.extend_from_slice(chunk);
        Ok(())
    }

    /// 弹出单个完整帧的载荷，如果完整帧
    /// 尚未被缓存，则返回 `Ok(None)`。
    pub fn try_pop(&mut self) -> io::Result<Option<Vec<u8>>> {
        let Some((consumed, payload)) = decode_frame(&self.buf)? else {
            return Ok(None);
        };
        let owned = payload.to_vec();
        self.buf.drain(..consumed);
        Ok(Some(owned))
    }

    /// 取出所有当前完整的帧。
    pub fn drain_all(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let mut frames = Vec::new();
        while let Some(payload) = self.try_pop()? {
            frames.push(payload);
        }
        Ok(frames)
    }

    /// 已缓存但尚未完整的字节数。
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_single_frame() {
        let payload = b"hello sinrana".to_vec();
        let wire = encode_frame(&payload);
        let (consumed, decoded) = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(decoded, payload);
    }

    #[test]
    fn decode_waits_for_partial_frame() {
        let payload = b"0123456789".to_vec();
        let wire = encode_frame(&payload);
        // 比完整帧少一个字节。
        let partial = &wire[..wire.len() - 1];
        assert!(decode_frame(partial).unwrap().is_none());
        assert!(decode_frame(&wire).unwrap().is_some());
    }

    #[test]
    fn codec_handles_multiple_frames_and_sticky_reads() {
        let mut codec = FrameCodec::new();
        let a = encode_frame(b"first");
        let b = encode_frame(b"second");
        codec.push(&a).unwrap();
        codec.push(&b).unwrap();
        let frames = codec.drain_all().unwrap();
        assert_eq!(frames, vec![b"first".to_vec(), b"second".to_vec()]);
        assert_eq!(codec.buffered(), 0);
    }

    #[test]
    fn codec_splits_one_byte_stream_across_pushes() {
        let mut codec = FrameCodec::new();
        let wire = encode_frame(b"split-across-reads");
        // 每次喂入一个字节。
        for byte in &wire {
            codec.push(std::slice::from_ref(byte)).unwrap();
        }
        let frames = codec.drain_all().unwrap();
        assert_eq!(frames, vec![b"split-across-reads".to_vec()]);
    }

    #[test]
    fn rejects_oversized_frame() {
        let big = (MAX_FRAME + 1) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&big.to_be_bytes());
        buf.extend_from_slice(&[0u8; 4]);
        assert!(decode_frame(&buf).is_err());
    }
}
