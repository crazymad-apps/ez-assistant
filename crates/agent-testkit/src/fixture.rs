//! Fixture 内容校验：拒绝真实凭据和敏感 header 进入仓库。
//!
//! fixture 必须可审阅、可重放：不包含真实 credential、用户内容或不可再现的
//! 动态值。写入 fixture 前用 [`validate_request`] / [`validate_response`] 把关；
//! 校验信息本身也绝不携带被检出的敏感值。

use std::fmt;

use crate::{BodyStep, RecordedRequest, RecordedResponse};

/// 一旦出现即判定 fixture 不可入库的 header 名（小写）。
const SENSITIVE_HEADERS: [&str; 5] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "cookie",
    "set-cookie",
];

/// body 文本中疑似 credential 的模式。保守策略：宁误拒不漏放，
/// fixture 文本被误伤时应改写为不触发模式的等价内容。
const CREDENTIAL_PATTERNS: [&str; 3] = ["Bearer ", "Basic ", "sk-"];

#[derive(Clone, Debug, Eq, PartialEq)]
/// fixture 校验发现的违规。
pub enum FixtureViolation {
    /// fixture 携带了敏感 header（应删除或替换为脱敏占位）。
    SensitiveHeader {
        /// 违规 header 名（header 名不是秘密，值才是）。
        name: String,
    },
    /// fixture 文本中出现疑似 credential 的模式。
    CredentialLikeValue {
        /// 出现位置描述（如 `request body`），不包含命中的值。
        location: String,
    },
}

impl fmt::Display for FixtureViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FixtureViolation::SensitiveHeader { name } => write!(
                formatter,
                "fixture must not contain sensitive header `{name}`"
            ),
            FixtureViolation::CredentialLikeValue { location } => write!(
                formatter,
                "fixture {location} contains a credential-like value"
            ),
        }
    }
}

impl std::error::Error for FixtureViolation {}

/// 校验录制的请求适合进入 fixture。
pub fn validate_request(request: &RecordedRequest) -> Result<(), FixtureViolation> {
    validate_headers(&request.headers)?;
    scan_bytes(&request.body, "request body")
}

/// 校验脚本化的响应适合进入 fixture。
pub fn validate_response(response: &RecordedResponse) -> Result<(), FixtureViolation> {
    validate_headers(&response.headers)?;
    for step in &response.body {
        if let BodyStep::Chunk(chunk) = step {
            scan_bytes(chunk, "response body")?;
        }
    }
    Ok(())
}

fn validate_headers(headers: &[(String, String)]) -> Result<(), FixtureViolation> {
    for (name, _) in headers {
        if SENSITIVE_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(FixtureViolation::SensitiveHeader { name: name.clone() });
        }
    }
    Ok(())
}

fn scan_bytes(bytes: &[u8], location: &str) -> Result<(), FixtureViolation> {
    let text = String::from_utf8_lossy(bytes);
    if CREDENTIAL_PATTERNS
        .iter()
        .any(|pattern| text.contains(pattern))
    {
        return Err(FixtureViolation::CredentialLikeValue {
            location: location.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_sensitive_headers_without_leaking_values() {
        let request = RecordedRequest::new("POST", "https://api.deepseek.com/chat/completions")
            .with_header("Authorization", "sk-real-secret-key");
        let violation = validate_request(&request).expect_err("must reject");
        assert_eq!(
            violation,
            FixtureViolation::SensitiveHeader {
                name: "Authorization".to_owned()
            }
        );
        // 违规信息只含 header 名，不含 credential 值。
        let display = violation.to_string();
        assert!(display.contains("Authorization"));
        assert!(!display.contains("sk-real-secret-key"));
    }

    #[test]
    fn rejects_credential_like_values_in_bodies() {
        let response = RecordedResponse::new(200, r#"{"token":"sk-abc123"}"#);
        assert_eq!(
            validate_response(&response).expect_err("must reject"),
            FixtureViolation::CredentialLikeValue {
                location: "response body".to_owned()
            }
        );
        let request =
            RecordedRequest::new("POST", "https://api.deepseek.com").with_body("Bearer abc");
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn accepts_clean_fixtures() {
        let request = RecordedRequest::new("POST", "https://api.deepseek.com/chat/completions")
            .with_header("content-type", "application/json")
            .with_body(br#"{"model":"deepseek-reasoner"}"#.to_vec());
        assert!(validate_request(&request).is_ok());
        let response = RecordedResponse::chunked(
            200,
            vec![
                BodyStep::Chunk(b"data: {\"choices\":[]}".to_vec()),
                BodyStep::Chunk(b"data: [DONE]\n\n".to_vec()),
            ],
        )
        .with_header("content-type", "text/event-stream");
        assert!(validate_response(&response).is_ok());
    }
}
