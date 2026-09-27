//! 插件原生配置文件的元数据读取与原样流式替换；正文不进入命令协议。

use std::{sync::Arc, time::Duration};

use assistant_protocol::{RuntimeErrorCode, RuntimeErrorInfo};
use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use super::{ReadyServices, error::runtime_status};
use crate::{access::AccessPermit, plugins::config_files};

#[derive(Deserialize)]
pub(super) struct ConfigQuery {
    sha256: String,
}

#[derive(Serialize)]
struct ConfigError {
    error: RuntimeErrorInfo,
}

pub(super) async fn list_files(
    Extension(services): Extension<Arc<ReadyServices>>,
    Extension(permit): Extension<AccessPermit>,
    Path(name): Path<String>,
    Query(query): Query<ConfigQuery>,
) -> Response {
    let _lease = match services
        .runtime
        .plugin_config_lease(&name, &query.sha256)
        .await
    {
        Ok(lease) => lease,
        Err(error) => return failure(error.to_protocol_info()),
    };
    if permit.check().is_err() {
        return unavailable();
    }
    let root = match config_files::config_root(&services.paths.user_root, &name) {
        Ok(root) => root,
        Err(_) => return unavailable(),
    };
    match tokio::task::spawn_blocking(move || config_files::list(&root)).await {
        Ok(Ok(files)) => (StatusCode::OK, Json(files)).into_response(),
        _ => unavailable(),
    }
}

pub(super) async fn upload_file(
    Extension(services): Extension<Arc<ReadyServices>>,
    Extension(permit): Extension<AccessPermit>,
    Path((name, relative)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(sha) = header(&headers, "x-ez-plugin-sha256") else {
        return invalid("plugin package SHA is required");
    };
    let Some(expected) = header(&headers, "x-ez-expected-revision") else {
        return invalid("configuration file revision is required");
    };
    if expected != "absent"
        && (expected.len() != 71
            || !expected.starts_with("sha256:")
            || !expected[7..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    {
        return invalid("configuration file revision is invalid");
    }
    let _lease = match services.runtime.plugin_config_lease(&name, sha).await {
        Ok(lease) => lease,
        Err(error) => return failure(error.to_protocol_info()),
    };
    let root = match config_files::config_root(&services.paths.user_root, &name) {
        Ok(root) => root,
        Err(_) => return unavailable(),
    };
    let target = match config_files::target(&root, relative.trim_start_matches('/'), true) {
        Ok(target) => target,
        Err(_) => return invalid("configuration path is invalid"),
    };
    if !matches_expected(&target, &relative, expected) {
        return conflict();
    }
    let temporary = match tempfile::NamedTempFile::new_in(&root) {
        Ok(temporary) => temporary,
        Err(_) => return unavailable(),
    };
    let file = match temporary.reopen() {
        Ok(file) => file,
        Err(_) => return unavailable(),
    };
    let mut file = tokio::fs::File::from_std(file);
    let mut stream = body.into_data_stream();
    let started = tokio::time::Instant::now();
    let mut size = 0_u64;
    loop {
        if started.elapsed() > Duration::from_secs(30 * 60) || services.cancellation.is_cancelled()
        {
            return unavailable();
        }
        let chunk = match tokio::time::timeout(Duration::from_secs(60), stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(None) => break,
            _ => return unavailable(),
        };
        size = match size
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= config_files::MAX_FILE_BYTES)
        {
            Some(size) => size,
            None => return invalid("configuration file exceeds size limit"),
        };
        if file.write_all(&chunk).await.is_err() {
            return unavailable();
        }
    }
    if file.flush().await.is_err() || file.sync_all().await.is_err() || permit.check().is_err() {
        return unavailable();
    }
    drop(file);
    if !matches_expected(&target, &relative, expected) {
        return conflict();
    }
    if temporary.persist(&target).is_err() {
        return unavailable();
    }
    if target
        .parent()
        .is_none_or(|parent| config_files::sync_directory(parent).is_err())
    {
        return unavailable();
    }
    let result = match config_files::current(&target, &relative) {
        Ok(Some(result)) if result.size_bytes == size => result,
        _ => return unavailable(),
    };
    services.runtime.notify_plugin_config_changed();
    (StatusCode::OK, Json(result)).into_response()
}

fn matches_expected(path: &std::path::Path, relative: &str, expected: &str) -> bool {
    match config_files::current(path, relative) {
        Ok(Some(current)) => current.revision == expected,
        Ok(None) => expected == "absent",
        Err(_) => false,
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn invalid(message: &'static str) -> Response {
    failure(RuntimeErrorInfo::new(
        RuntimeErrorCode::InvalidRequest,
        message,
    ))
}
fn conflict() -> Response {
    failure(RuntimeErrorInfo::new(
        RuntimeErrorCode::PluginBusy,
        "configuration file changed",
    ))
}
fn unavailable() -> Response {
    failure(RuntimeErrorInfo::new(
        RuntimeErrorCode::StorageUnavailable,
        "plugin configuration storage is unavailable",
    ))
}
fn failure(error: RuntimeErrorInfo) -> Response {
    (runtime_status(error.code), Json(ConfigError { error })).into_response()
}
