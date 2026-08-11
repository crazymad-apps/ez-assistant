//! Attachment Blob 身份摘要的唯一编码规则。

use sha2::{Digest, Sha256};

/// 域分隔符固定摘要语义，避免未来把普通文件内容摘要误当成 Blob 身份。
const HASH_DOMAIN: &[u8] = b"ez-assistant-attachment-v1\0";

/// 创建已写入原始文件名帧的 SHA-256；调用方随后按顺序写入文件字节。
///
/// 文件名长度使用大端 u64 编码，避免文件名与正文直接拼接产生边界歧义。
pub(crate) fn new_hasher(original_name: &str) -> Sha256 {
    let name = original_name.as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(HASH_DOMAIN);
    hasher.update((name.len() as u64).to_be_bytes());
    hasher.update(name);
    hasher
}

#[cfg(test)]
pub(crate) fn digest_bytes(original_name: &str, bytes: &[u8]) -> String {
    let mut hasher = new_hasher(original_name);
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::digest_bytes;

    #[test]
    fn blob_identity_depends_on_name_and_bytes() {
        assert_eq!(
            digest_bytes("a.txt", b"same"),
            digest_bytes("a.txt", b"same")
        );
        assert_ne!(
            digest_bytes("a.txt", b"same"),
            digest_bytes("b.txt", b"same")
        );
        assert_ne!(
            digest_bytes("a.txt", b"same"),
            digest_bytes("a.txt", b"different")
        );
    }
}
