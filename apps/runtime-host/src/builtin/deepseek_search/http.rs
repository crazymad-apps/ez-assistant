//! Search HTTP 契约与有限重试；不读取文件、模型配置、原生 JSON 或全局凭据。

use std::{
    net::IpAddr,
    time::{Duration, SystemTime},
};

use reqwest::{
    Client, StatusCode,
    header::{HeaderValue, RETRY_AFTER},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

const ATTEMPT_BUDGET: Duration = Duration::from_secs(30);
const RESPONSE_LIMIT: usize = 10 * 1024 * 1024;
const MAX_RETRIES: u32 = 2;

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchInput {
    /// 搜索语句，保留站点筛选等搜索语法，1～128 个字符。
    #[schemars(length(min = 1, max = 128))]
    pub(super) query: String,
    /// 返回条数，1～50，默认 10。
    #[schemars(range(min = 1, max = 50))]
    pub(super) count: Option<u32>,
    #[schemars(length(min = 1, max = 128))]
    pub(super) ori_question: Option<String>,
    /// 定位 IP；省略或空串使用服务默认值。
    pub(super) localization: Option<String>,
    /// 是否附网页完整正文，默认 false；大正文建议减少 count。
    pub(super) need_content: Option<bool>,
}

impl SearchInput {
    pub(super) fn validate(&self) -> Result<(), SearchError> {
        if self.query.trim().is_empty()
            || self.query.chars().count() > 128
            || self.count.is_some_and(|count| !(1..=50).contains(&count))
            || self
                .ori_question
                .as_ref()
                .is_some_and(|text| text.is_empty() || text.chars().count() > 128)
            || self
                .localization
                .as_ref()
                .is_some_and(|ip| !ip.is_empty() && ip.parse::<IpAddr>().is_err())
        {
            return Err(SearchError::InvalidInput);
        }
        Ok(())
    }

    fn payload(&self) -> Value {
        let mut value = json!({"query":self.query,"count":self.count.unwrap_or(10),"localization":self.localization.as_deref().unwrap_or(""),"need_content":self.need_content.unwrap_or(false)});
        if let Some(question) = &self.ori_question {
            value["ori_question"] = json!(question);
        }
        value
    }
}

/// 全部变体是静态安全原因，绝不包含 reqwest 原始错误、响应正文或 Authorization。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SearchError {
    InvalidInput,
    MissingKey,
    InvalidConfiguration,
    Unauthorized,
    Balance,
    RateLimited,
    Timeout,
    Network,
    Http,
    InvalidResponse,
    ResponseTooLarge,
    Cancelled,
    Busy,
}

impl SearchError {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::InvalidInput => {
                "DeepSeek 搜索参数无效：query/ori_question 为 1～128 字符，query 不能全空白；count 为 1～50；localization 为 IP 或空串。"
            }
            Self::MissingKey => {
                "请在 DeepSeek 搜索详情配置 DEEPSEEK_API_KEY；不会读取模型 Key、原生 JSON 或其他来源。"
            }
            Self::InvalidConfiguration => {
                "DeepSeek 搜索配置无效，请检查 API Key 和 DEEPSEEK_BASE_URL。"
            }
            Self::Unauthorized => "DeepSeek API Key 缺失或无效，请在服务详情重新配置。",
            Self::Balance => "DeepSeek 账号余额不足。",
            Self::RateLimited => "DeepSeek 搜索受到限流，有限重试已结束。",
            Self::Timeout => "DeepSeek 搜索请求超时，有限重试已结束。",
            Self::Network => "DeepSeek 搜索网络连接失败，有限重试已结束。",
            Self::Http => "DeepSeek 搜索服务拒绝请求或暂不可用。",
            Self::InvalidResponse => "DeepSeek 搜索返回了无效 JSON 或不符合接口结构的结果。",
            Self::ResponseTooLarge => {
                "DeepSeek 搜索响应超过大小上限，请减少 count 或关闭 need_content。"
            }
            Self::Cancelled => "DeepSeek 搜索已取消。",
            Self::Busy => "DeepSeek 搜索正在处理 4 个请求，请稍后重试。",
        }
    }
}

/// 每个 MCP 工具进程的固定 HTTP 装配，无后台任务和历史缓存；HeaderValue 标记敏感。
pub(super) struct SearchClient {
    http: Client,
    endpoint: Url,
    authorization: Option<HeaderValue>,
    attempt_budget: Duration,
    response_limit: usize,
}

/// 一次 attempt 的可重试事实，只在当前调用中存在。
struct AttemptFailure {
    error: SearchError,
    retryable: bool,
    retry_after: Option<Duration>,
}

impl AttemptFailure {
    fn final_error(error: SearchError) -> Self {
        Self {
            error,
            retryable: false,
            retry_after: None,
        }
    }
    fn network(error: reqwest::Error) -> Self {
        Self {
            error: if error.is_timeout() {
                SearchError::Timeout
            } else {
                SearchError::Network
            },
            retryable: true,
            retry_after: None,
        }
    }
}

impl SearchClient {
    pub(super) fn new(key: Option<&str>, base: Option<&str>) -> Result<Self, SearchError> {
        let base = base
            .filter(|base| !base.trim().is_empty())
            .unwrap_or("https://api.deepseek.com");
        let mut endpoint = Url::parse(base).map_err(|_| SearchError::InvalidConfiguration)?;
        let loopback = endpoint.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if (endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && loopback))
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(SearchError::InvalidConfiguration);
        }
        let prefix = endpoint
            .path()
            .trim_end_matches('/')
            .trim_end_matches("/v1");
        let path = format!("{prefix}/v1/tools/search");
        endpoint.set_path(&path);
        let authorization = key
            .filter(|key| !key.trim().is_empty())
            .map(|key| {
                let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| SearchError::InvalidConfiguration)?;
                value.set_sensitive(true);
                Ok(value)
            })
            .transpose()?;
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| SearchError::InvalidConfiguration)?;
        Ok(Self {
            http,
            endpoint,
            authorization,
            attempt_budget: ATTEMPT_BUDGET,
            response_limit: RESPONSE_LIMIT,
        })
    }

    /// 每次包含响应读取的预算为 30 秒；最多两次重试及两次各不超过 30 秒的等待，总预算不超过 150 秒。
    /// 取消丢弃当前 reqwest future，停止读取／退避；外层不启动脱离 owner 的 HTTP 任务。
    pub(super) async fn search(
        &self,
        input: &SearchInput,
        cancellation: &CancellationToken,
    ) -> Result<Value, SearchError> {
        input.validate()?;
        if self.authorization.is_none() {
            return Err(SearchError::MissingKey);
        }
        for attempt in 0..=MAX_RETRIES {
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(SearchError::Cancelled),
                result = tokio::time::timeout(self.attempt_budget, self.attempt(input)) => {
                    result.unwrap_or(Err(AttemptFailure {
                        error: SearchError::Timeout,
                        retryable: true,
                        retry_after: None,
                    }))
                }
            };
            match result {
                Ok(value) => return Ok(value),
                Err(failure) if !failure.retryable || attempt == MAX_RETRIES => {
                    return Err(failure.error);
                }
                Err(failure) => {
                    let delay = failure.retry_after.unwrap_or_else(|| {
                        Duration::from_millis(250_u64.saturating_mul(1_u64 << attempt))
                    });
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Err(SearchError::Cancelled),
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
        Err(SearchError::Http)
    }

    async fn attempt(&self, input: &SearchInput) -> Result<Value, AttemptFailure> {
        let authorization = self
            .authorization
            .as_ref()
            .ok_or_else(|| AttemptFailure::final_error(SearchError::MissingKey))?;
        let mut response = self
            .http
            .post(self.endpoint.clone())
            .header(reqwest::header::AUTHORIZATION, authorization.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&input.payload())
            .send()
            .await
            .map_err(AttemptFailure::network)?;
        let status = response.status();
        if status != StatusCode::OK {
            let error = match status.as_u16() {
                401 => SearchError::Unauthorized,
                402 => SearchError::Balance,
                429 => SearchError::RateLimited,
                _ => SearchError::Http,
            };
            let retryable = matches!(status.as_u16(), 408 | 429) || status.is_server_error();
            return Err(AttemptFailure {
                error,
                retryable,
                retry_after: response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(retry_after),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.response_limit as u64)
        {
            return Err(AttemptFailure::final_error(SearchError::ResponseTooLarge));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(AttemptFailure::network)? {
            if bytes.len().saturating_add(chunk.len()) > self.response_limit {
                return Err(AttemptFailure::final_error(SearchError::ResponseTooLarge));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| AttemptFailure::final_error(SearchError::InvalidResponse))?;
        validate_response(&value).map_err(AttemptFailure::final_error)?;
        Ok(value)
    }
}

fn retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(30)));
    }
    httpdate::parse_http_date(value).ok().map(|date| {
        date.duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
            .min(Duration::from_secs(30))
    })
}

fn validate_response(value: &Value) -> Result<(), SearchError> {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or(SearchError::InvalidResponse)?;
    for item in results {
        if !["title", "url", "snippet", "summary", "content"]
            .iter()
            .all(|field| item.get(field).is_some_and(Value::is_string))
            || !["site_name", "site_icon", "date"].iter().all(|field| {
                item.get(field)
                    .is_some_and(|value| value.is_null() || value.is_string())
            })
            || !item
                .get("rerank_score")
                .and_then(Value::as_f64)
                .is_some_and(|score| (0.0..=3.0).contains(&score))
            || !item
                .get("authority")
                .and_then(Value::as_i64)
                .is_some_and(|authority| (-1..=4).contains(&authority))
        {
            return Err(SearchError::InvalidResponse);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
