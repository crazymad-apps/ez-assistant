//! 在线发现只持有本次响应；分页任一步失败丢弃全部结果，不缓存、不重试、不改变调用方选择。

#[cfg(test)]
mod live_tests;
mod parse;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use assistant_runtime::{
    DiscoveredModel, ModelDiscoveryError, ModelDiscoveryErrorKind as ErrorKind,
    ModelDiscoveryFormat as DiscoveryFormat, ModelDiscoveryRequest,
};
use reqwest::{Client, StatusCode, Url};

// 限制单次发现的内存和网络工作量；超过边界显式失败，绝不截断成成功列表。
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 8 * MAX_RESPONSE_BYTES;
const MAX_MODELS: usize = 10_000;
const MAX_PAGES: u64 = 100;
const PAGE_SIZE: u64 = 100;

/// 此 future 拥有全部在途请求和分页进度；外层丢弃它即可取消，不生成独立任务。
pub(super) async fn discover(
    request: ModelDiscoveryRequest<'_>,
) -> Result<Vec<DiscoveredModel>, ModelDiscoveryError> {
    if request.connect_timeout.is_zero() || request.request_timeout.is_zero() {
        return Err(ModelDiscoveryError::new(ErrorKind::InvalidConfiguration));
    }
    let (endpoint, format) = endpoint(&request)?;
    let client = Client::builder()
        .connect_timeout(request.connect_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(transport_error)?;
    // 总预算覆盖全部分页和 body 读取，避免服务商持续发送小块数据无限延长请求。
    tokio::time::timeout(
        request.request_timeout,
        fetch_all(&client, endpoint, format, request.api_key),
    )
    .await
    .map_err(|_| ModelDiscoveryError::new(ErrorKind::Timeout))?
}

fn endpoint(
    request: &ModelDiscoveryRequest<'_>,
) -> Result<(Url, DiscoveryFormat), ModelDiscoveryError> {
    let path = request.models_path;
    if !path.is_empty()
        && (!path.starts_with('/')
            || path.starts_with("//")
            || path.len() > 2048
            || !path
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte))
            || path.split('/').any(|part| part == "." || part == ".."))
    {
        return Err(ModelDiscoveryError::new(ErrorKind::InvalidConfiguration));
    }
    let mut url = Url::parse(request.endpoint)
        .map_err(|_| ModelDiscoveryError::new(ErrorKind::InvalidConfiguration))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ModelDiscoveryError::new(ErrorKind::InvalidConfiguration));
    }
    // 未覆盖时由适配器选择唯一默认路径；兼容接口沿用推理 Endpoint 的基础路径。
    // 不把默认值写回用户配置，也不在请求失败后盲试其他路径。
    if path.is_empty() {
        let default_path = match request.format {
            DiscoveryFormat::DashScope => "/api/v1/models".to_owned(),
            DiscoveryFormat::OpenAi | DiscoveryFormat::Vllm | DiscoveryFormat::Moonshot => {
                format!("{}/models", url.path().trim_end_matches('/'))
            }
        };
        url.set_path(&default_path);
    } else {
        url.set_path(path);
    }
    Ok((url, request.format))
}

async fn fetch_all(
    client: &Client,
    endpoint: Url,
    format: DiscoveryFormat,
    api_key: &str,
) -> Result<Vec<DiscoveredModel>, ModelDiscoveryError> {
    let mut models = Vec::new();
    let mut identifiers = BTreeSet::new();
    let mut total_bytes = 0;
    let mut expected_total = None;
    for page_no in 1..=MAX_PAGES {
        let mut page_url = endpoint.clone();
        if matches!(format, DiscoveryFormat::DashScope) {
            page_url
                .query_pairs_mut()
                .append_pair("page_no", &page_no.to_string())
                .append_pair("page_size", &PAGE_SIZE.to_string());
        }
        let mut request = client.get(page_url);
        if !api_key.is_empty() {
            request = request.bearer_auth(api_key);
        }
        let response = request.send().await.map_err(transport_error)?;
        check_status(response.status())?;
        let bytes = read_body(response, &mut total_bytes).await?;
        let page = parse::page(&bytes, format, page_no)?;
        if let Some(total) = page.total {
            if expected_total.is_some_and(|expected| expected != total) {
                return Err(ModelDiscoveryError::new(ErrorKind::InvalidResponse));
            }
            expected_total = Some(total);
        }
        if models.len() + page.models.len() > MAX_MODELS {
            return Err(ModelDiscoveryError::new(ErrorKind::ResponseTooLarge));
        }
        for model in page.models {
            if !identifiers.insert(model.model_id.clone()) {
                return Err(ModelDiscoveryError::new(ErrorKind::InvalidResponse));
            }
            models.push(model);
        }
        match expected_total {
            None => return Ok(models),
            Some(total) if models.len() == total => return Ok(models),
            Some(total) if models.len() > total => {
                return Err(ModelDiscoveryError::new(ErrorKind::InvalidResponse));
            }
            _ => {}
        }
    }
    Err(ModelDiscoveryError::new(ErrorKind::ResponseTooLarge))
}

async fn read_body(
    mut response: reqwest::Response,
    total_bytes: &mut usize,
) -> Result<Vec<u8>, ModelDiscoveryError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(ModelDiscoveryError::new(ErrorKind::ResponseTooLarge));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len()
            || chunk.len() > MAX_TOTAL_BYTES - *total_bytes
        {
            return Err(ModelDiscoveryError::new(ErrorKind::ResponseTooLarge));
        }
        *total_bytes += chunk.len();
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn check_status(status: StatusCode) -> Result<(), ModelDiscoveryError> {
    if status.is_success() {
        return Ok(());
    }
    let kind = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ErrorKind::Authentication,
        StatusCode::TOO_MANY_REQUESTS => ErrorKind::RateLimited,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => ErrorKind::Unsupported,
        _ => ErrorKind::Unavailable,
    };
    // 不读取错误 body，避免把服务商回显的凭据或响应正文带入错误消息。
    Err(ModelDiscoveryError::new(kind))
}

fn transport_error(error: reqwest::Error) -> ModelDiscoveryError {
    let kind = if error.is_timeout() {
        ErrorKind::Timeout
    } else if error.is_builder() {
        ErrorKind::InvalidConfiguration
    } else {
        ErrorKind::Unavailable
    };
    ModelDiscoveryError::with_source(kind, error.without_url())
}
