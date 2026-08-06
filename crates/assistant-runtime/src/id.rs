//! Runtime 拥有的短随机业务标识生成。

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

const RANDOM_ID_BYTES: usize = 9;

/// 生成 `<prefix>_<12 chars>` 形式的 URL-safe 不透明标识。
pub(crate) fn generate(prefix: &str) -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; RANDOM_ID_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_short_prefixed_and_distinct() {
        let first = generate("s").expect("system random source");
        let second = generate("s").expect("system random source");

        assert_eq!(first.len(), 14);
        assert!(first.starts_with("s_"));
        assert_ne!(first, second);
        assert!(
            first
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') })
        );
    }
}
