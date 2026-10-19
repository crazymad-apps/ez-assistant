//! OpenCode 要求的不可变请求身份；不解释宿主 Session 或业务状态。

/// 只用于 OpenCode 方言的会话路由与客户端身份。
#[derive(Clone)]
pub struct OpenCodeRequestIdentity {
    session: String,
    user_agent: String,
}

impl OpenCodeRequestIdentity {
    /// 绑定宿主已派生的路由标识与实际产品 User-Agent。
    ///
    /// # Errors
    /// 空值、控制字符、非 ASCII 或超长值返回配置错误，不回显输入。
    pub fn new(session: &str, user_agent: &str) -> Result<Self, agent_model::ModelError> {
        let valid = |value: &str, limit| {
            !value.is_empty()
                && value.len() <= limit
                && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        };
        if !valid(session, 128) || !valid(user_agent, 256) {
            return Err(agent_model::ModelError::Config(
                "invalid OpenCode request identity".to_owned(),
            ));
        }
        Ok(Self {
            session: session.to_owned(),
            user_agent: user_agent.to_owned(),
        })
    }

    pub(crate) fn append_headers(&self, headers: &mut Vec<(String, String)>) {
        headers.push(("x-opencode-session".to_owned(), self.session.clone()));
        headers.push(("user-agent".to_owned(), self.user_agent.clone()));
    }
}

pub(crate) fn apply_identity(
    provider: &agent_types::ProviderId,
    identity: Option<&OpenCodeRequestIdentity>,
    headers: &mut Vec<(String, String)>,
) -> Result<(), agent_model::ModelError> {
    if provider.as_str() == "opencode" {
        identity
            .ok_or_else(|| {
                agent_model::ModelError::Config(
                    "OpenCode requires a conversation routing identity".to_owned(),
                )
            })?
            .append_headers(headers);
    }
    Ok(())
}
