//! 媒体链路诊断只输出不可逆关联标识、时间和计数，不记录语音、正文或凭据。

use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// 外部 ID 可能包含控制字符或正文；日志只保留固定长度摘要，绝不直接插值外部字符串。
pub(crate) fn correlation_id(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    )
}

pub(crate) fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_is_stable_bounded_and_never_copies_untrusted_text() {
        let input = "secret\n event=forged 输入";
        let id = correlation_id(input);
        assert_eq!(id, correlation_id(input));
        assert_ne!(id, correlation_id("another"));
        assert_eq!(id.len(), 12);
        assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!id.contains("secret"));
    }
}
