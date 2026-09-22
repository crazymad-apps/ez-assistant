//! 中心透明代理的控制响应适配；其余 wire 字节与状态完全交给原 Adapter。

use agent_model::ModelError;
use agent_openai_compatible::{
    ReqwestTransport, Transport, TransportError, TransportFuture, TransportRequest,
    TransportResponse,
};
use futures_util::StreamExt;

use super::*;

pub(super) struct CenterModelTransport {
    loader: Arc<CenterModelLoader>,
    configuration: Arc<ExternalModelConfiguration>,
    inner: ReqwestTransport,
    endpoint: url::Url,
}

impl CenterModelTransport {
    pub(super) fn new(
        loader: Arc<CenterModelLoader>,
        configuration: Arc<ExternalModelConfiguration>,
        request: &ModelServiceFactoryRequest<'_>,
    ) -> Result<Self, ModelServiceFactoryError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(request.connect_timeout)
            .build()
            .map_err(|_| ModelServiceFactoryError::new("center transport is unavailable"))?;
        let endpoint = url::Url::parse(request.endpoint)
            .map_err(|_| ModelServiceFactoryError::new("invalid center endpoint"))?;
        Ok(Self {
            loader,
            configuration,
            inner: ReqwestTransport::with_client(client, request.request_timeout),
            endpoint,
        })
    }

    async fn send(
        &self,
        mut request: TransportRequest,
    ) -> Result<TransportResponse, TransportError> {
        let (domain, login) = self
            .loader
            .login()
            .map_err(|_| reject(ModelError::Auth("登录已失效，请重新登录。".to_owned())))?;
        let trusted = url::Url::parse(login.center_url())
            .map_err(|_| reject(ModelError::Config("中心地址无效。".to_owned())))?;
        let target = url::Url::parse(&request.url)
            .map_err(|_| reject(ModelError::Config("模型请求地址无效。".to_owned())))?;
        if target.origin() != trusted.origin()
            || target.origin() != self.endpoint.origin()
            || ![
                format!("{}/responses", self.endpoint.path().trim_end_matches('/')),
                format!(
                    "{}/chat/completions",
                    self.endpoint.path().trim_end_matches('/')
                ),
            ]
            .contains(&target.path().to_owned())
        {
            return Err(reject(ModelError::Config(
                "模型请求不属于受信中心。".to_owned(),
            )));
        }
        request
            .headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("authorization"));
        request
            .headers
            .push(("authorization".to_owned(), login.model_authorization()));
        let mut response = self.inner.execute(request).await?;
        if !response
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("x-ez-center-control") && value == "1")
        {
            return Ok(response);
        }
        if !matches!(response.status, 400 | 401 | 409 | 413 | 503) {
            return Ok(response);
        }
        // 标记但未知的错误仍交原 Adapter。保留 chunk 边界，不对普通 SSE 聚合。
        let mut chunks = Vec::new();
        let mut body = Vec::new();
        let mut complete = true;
        while let Some(chunk) = response.body.next().await {
            let bytes = match &chunk {
                Ok(bytes) => bytes,
                Err(_) => {
                    complete = false;
                    chunks.push(chunk);
                    break;
                }
            };
            if body.len() + bytes.len() > 65536 {
                complete = false;
                chunks.push(chunk);
                break;
            }
            body.extend_from_slice(bytes);
            chunks.push(chunk);
        }
        let code = complete
            .then(|| serde_json::from_slice::<serde_json::Value>(&body).ok())
            .flatten()
            .and_then(|value| value.get("error")?.get("code")?.as_str().map(str::to_owned));
        let error = match (response.status, code.as_deref()) {
            (409, Some("managed_model_configuration_stale" | "managed_model_unavailable")) => {
                let message = match self
                    .loader
                    .rejected_configuration(&self.configuration)
                    .await
                {
                    Ok(()) => "中心模型配置已刷新，请重试；若仍不可用，请联系管理员。",
                    Err(_) => "中心模型配置已失效，刷新失败，请稍后刷新后重试。",
                };
                Some(ModelError::Config(message.to_owned()))
            }
            (401, Some("llm_key_invalid")) => {
                self.loader
                    .credentials
                    .reject_model_login(&self.loader.key, &domain, &login);
                Some(ModelError::Auth("模型凭据已失效，请重新登录。".to_owned()))
            }
            (400, Some("llm_request_invalid"))
            | (413, Some("llm_request_too_large"))
            | (503, Some("llm_record_unavailable" | "llm_proxy_unavailable")) => {
                Some(ModelError::Provider {
                    message: "中心未接纳本次模型请求，请稍后重试或联系管理员。".to_owned(),
                    status: Some(response.status),
                })
            }
            _ => None,
        };
        if let Some(error) = error {
            return Err(reject(error));
        }
        response.body = Box::pin(futures_util::stream::iter(chunks).chain(response.body));
        Ok(response)
    }
}

impl Transport for CenterModelTransport {
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
        Box::pin(async move {
            tokio::select! {
                () = self.loader.cancellation.cancelled() => Err(reject(ModelError::Auth("用户服务已关闭。".to_owned()))),
                result = self.send(request) => result,
            }
        })
    }
}

fn reject(error: ModelError) -> TransportError {
    TransportError::Rejected(error)
}
