//! 当前登录用户的企业智能体完整目录，拒绝部分替换和无界响应。

use std::time::Duration;

use assistant_protocol::{CustomAgentConfig, SecretValue};

use super::CenterClient;

impl CenterClient {
    pub(crate) async fn list_runtime_agents(
        &self,
        token: &SecretValue,
    ) -> Result<Vec<CustomAgentConfig>, super::CenterError> {
        let mut url =
            reqwest::Url::parse(&self.url).map_err(|_| super::CenterError::Unavailable)?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| super::CenterError::Unavailable)?;
            path.pop_if_empty();
            path.extend(["api", "runtime", "agents"]);
        }
        let mut response = self
            .client
            .get(url)
            .bearer_auth(token.expose())
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|_| super::CenterError::Unavailable)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > 16 * 1024 * 1024)
        {
            return Err(super::CenterError::Unavailable);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| super::CenterError::Unavailable)?
        {
            if bytes.len().saturating_add(chunk.len()) > 16 * 1024 * 1024 {
                return Err(super::CenterError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        let agents: Vec<CustomAgentConfig> =
            serde_json::from_slice(&bytes).map_err(|_| super::CenterError::Unavailable)?;
        if agents.len() > 1024
            || !agents
                .iter()
                .all(assistant_runtime::valid_custom_agent_config)
        {
            return Err(super::CenterError::Unavailable);
        }
        let mut names = std::collections::BTreeSet::new();
        if !agents.iter().all(|agent| names.insert(&agent.name)) {
            return Err(super::CenterError::Unavailable);
        }
        Ok(agents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};

    #[tokio::test]
    async fn runtime_agent_directory_is_full_and_rejects_duplicate_names() {
        let router = Router::new().route("/api/runtime/agents", get(|| async {
            Json(serde_json::json!([{
                "name":"企业审阅助手","description":"审阅","prompt":"请审阅。","skills":[],"mcp":[]
            }]))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = CenterClient::new(format!("http://{address}")).unwrap();
        let token = SecretValue::new("token".into());
        let agents = client.list_runtime_agents(&token).await.unwrap();
        assert_eq!(agents[0].name, "企业审阅助手");
        server.abort();

        let router = Router::new().route(
            "/api/runtime/agents",
            get(|| async {
                Json(serde_json::json!([
                    {"name":"重复","description":"a","prompt":"规则","skills":[],"mcp":[]},
                    {"name":"重复","description":"b","prompt":"规则","skills":[],"mcp":[]}
                ]))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = CenterClient::new(format!("http://{address}")).unwrap();
        assert!(client.list_runtime_agents(&token).await.is_err());
        server.abort();
    }
}
