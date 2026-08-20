//! Provider base URL 校验与稳定 endpoint 拼接。

use sha2::{Digest, Sha256};

/// 验证 base URL 不会把 credential 或非路径配置带入可记录 URL。
pub(crate) fn validate_base_url(base_url: String) -> Result<String, &'static str> {
    let parsed = reqwest::Url::parse(&base_url).map_err(|_| "must be a valid absolute URL")?;
    if parsed.cannot_be_a_base() {
        return Err("must support path joining");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("userinfo is not allowed");
    }
    if parsed.query().is_some() {
        return Err("query is not allowed");
    }
    if parsed.fragment().is_some() {
        return Err("fragment is not allowed");
    }
    Ok(base_url)
}

/// 在已验证的 base URL 后追加固定协议路径。
pub(crate) fn join_endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// 计算不含 credential 和 endpoint 明文的精确模型路由指纹。
pub(crate) fn route_fingerprint(
    provider: &str,
    protocol: &str,
    base_url: &str,
    model: &str,
) -> String {
    let mut url = reqwest::Url::parse(base_url).expect("base URL was validated before binding");
    let trimmed_path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&trimmed_path);
    let normalized = url.as_str().trim_end_matches('/');
    let mut digest = Sha256::new();
    for component in [provider, protocol, normalized, model] {
        let length = u64::try_from(component.len()).expect("route component length fits u64");
        digest.update(length.to_be_bytes());
        digest.update(component.as_bytes());
    }
    let bytes = digest.finalize();
    let mut fingerprint = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        fingerprint.push(char::from(HEX[usize::from(byte >> 4)]));
        fingerprint.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    fingerprint
}

#[cfg(test)]
mod tests {
    use super::route_fingerprint;

    #[test]
    fn route_fingerprint_normalizes_equivalent_urls_and_separates_routes() {
        let first = route_fingerprint(
            "deepseek",
            "openai.responses",
            "HTTPS://API.EXAMPLE.COM:443/v1/",
            "model-a",
        );
        let equivalent = route_fingerprint(
            "deepseek",
            "openai.responses",
            "https://api.example.com/v1",
            "model-a",
        );
        assert_eq!(first, equivalent);
        assert_eq!(first.len(), 64);
        assert_ne!(
            first,
            route_fingerprint(
                "deepseek",
                "openai.responses",
                "https://api.example.com/other",
                "model-a",
            )
        );
        assert_ne!(
            first,
            route_fingerprint(
                "deepseek",
                "openai.responses",
                "https://api.example.com/v1",
                "model-b",
            )
        );
    }
}
