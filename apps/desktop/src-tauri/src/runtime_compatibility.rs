//! Desktop 的本机、远端和原生资源 HTTP 请求共同携带编译软件版本。

pub(crate) fn headers() -> reqwest::header::HeaderMap {
    use assistant_protocol::{
        CLIENT_VERSION_HEADER, MIN_COMPATIBLE_VERSION, MIN_COMPATIBLE_VERSION_HEADER,
        SOFTWARE_VERSION,
    };
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    headers.insert(
        CLIENT_VERSION_HEADER,
        HeaderValue::from_static(SOFTWARE_VERSION),
    );
    headers.insert(
        MIN_COMPATIBLE_VERSION_HEADER,
        HeaderValue::from_static(MIN_COMPATIBLE_VERSION),
    );
    headers
}
