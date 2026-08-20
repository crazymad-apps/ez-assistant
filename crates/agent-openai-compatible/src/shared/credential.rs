//! Provider credential 的脱敏持有与认证头编码。

use std::fmt;

#[derive(Clone)]
/// OpenAI-compatible Bearer credential。
///
/// 只允许由协议服务写入 `Authorization` header；`Debug` 输出脱敏，
/// credential 不进入请求 DTO、事件和任何错误文本。
pub struct BearerCredential(String);

impl BearerCredential {
    /// 用 bearer token 创建凭据。
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// 写入 `Authorization` header 的值。
    pub(crate) fn authorization_header(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl fmt::Debug for BearerCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerCredential(<redacted>)")
    }
}
