//! Runtime 拥有的短随机业务标识生成。

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

const RANDOM_ID_BYTES: usize = 9;
pub(crate) const GENERATION_ATTEMPTS: usize = 16;

/// 生成 `<prefix>_<12 chars>` 形式的 URL-safe 不透明标识。
pub(crate) fn generate(prefix: &str) -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; RANDOM_ID_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// 新身份在可靠接纳前避开调用方提供的当前 owner 历史和实际控制；不持有或缓存历史。
/// 达到原有尝试上限即失败，不允许覆盖已知身份或无限重试。
///
/// # Errors
/// 随机源不可用或原有尝试预算内没有可用身份时失败。
pub(crate) fn generate_unique(
    prefix: &str,
    occupied: impl Fn(&str) -> bool,
) -> crate::RuntimeResult<String> {
    generate_unique_with(occupied, || {
        generate(prefix).map_err(|_| crate::RuntimeError::InternalStateUnavailable {
            component: "identifier random source",
        })
    })
}

fn generate_unique_with(
    occupied: impl Fn(&str) -> bool,
    mut candidate: impl FnMut() -> crate::RuntimeResult<String>,
) -> crate::RuntimeResult<String> {
    for _ in 0..GENERATION_ATTEMPTS {
        let value = candidate()?;
        if !occupied(&value) {
            return Ok(value);
        }
    }
    Err(crate::RuntimeError::InternalStateUnavailable {
        component: "identifier collision",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_history_id_is_resampled_without_reusing_it() {
        let mut values = ["inherited", "queued", "fresh"].into_iter();
        let value = generate_unique_with(
            |id| matches!(id, "inherited" | "queued"),
            || Ok(values.next().unwrap().to_owned()),
        )
        .unwrap();
        assert_eq!(value, "fresh");
        let mut attempts = 0;
        assert!(
            generate_unique_with(
                |_| true,
                || {
                    attempts += 1;
                    Ok("occupied".to_owned())
                }
            )
            .is_err()
        );
        assert_eq!(attempts, GENERATION_ATTEMPTS);
    }

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
