//! Host 本机管理 IPC。读取没有副作用；写入持实例锁并先完成布局升级，不启动业务 Runtime。

use super::{
    AccessError, Credentials, check_revision,
    config::{load, save, validate},
};
use crate::{
    config::{AccessArguments, AccessOperation},
    config_source::{LocalConfigSource, prepare_runtime_home},
    endpoint::{EndpointError, RuntimeInstanceGuard},
};
use assistant_protocol::{HostAccessConfiguration, SecretValue};
use serde::{Deserialize, Serialize};
use std::{error::Error, path::Path};
use tokio::io::AsyncReadExt as _;

const MAX_INPUT: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configure {
    expected_revision: Option<String>,
    configuration: HostAccessConfiguration,
    #[serde(default)]
    mode: Option<crate::host_configuration::HostMode>,
    #[serde(default)]
    center_url: Option<String>,
    #[serde(default)]
    clear_center_binding: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Password {
    expected_revision: Option<String>,
    password: SecretValue,
}
#[derive(Serialize)]
struct Status {
    mode: Option<assistant_protocol::HostMode>,
    center_url: Option<String>,
    center_id: Option<String>,
    revision: Option<String>,
    password_configured: bool,
    configuration: HostAccessConfiguration,
    effective_on_next_start: bool,
}

pub(crate) async fn run(arguments: AccessArguments) -> Result<(), Box<dyn Error>> {
    let result = if arguments.operation == AccessOperation::Probe {
        probe(&arguments).map(|held| serde_json::json!({"lock_held":held}).to_string())
    } else {
        match execute(arguments).await {
            Ok(status) => serde_json::to_string(&status)
                .map_err(|_| AccessError::Unavailable)
                .and_then(|output| {
                    if output.len() as u64 > MAX_INPUT {
                        Err(AccessError::Invalid("配置响应超过 64 KiB。"))
                    } else {
                        Ok(output)
                    }
                }),
            Err(error) => Err(error),
        }
    };
    match result {
        Ok(output) => {
            println!("{output}");
            Ok(())
        }
        Err(error) => {
            let code = match &error {
                AccessError::Conflict => "configuration_conflict",
                AccessError::Busy => "busy",
                AccessError::Unauthorized => "host_running",
                AccessError::PortInUse(_) => "port_in_use",
                AccessError::Invalid(_) => "invalid_request",
                AccessError::Unavailable | AccessError::Center(_) => "configuration_unavailable",
            };
            println!(
                "{}",
                serde_json::json!({"error":{"code":code,"message":error.to_string()}})
            );
            Err(Box::new(error))
        }
    }
}

async fn execute(arguments: AccessArguments) -> Result<Status, AccessError> {
    let home = arguments
        .runtime_home()
        .map_err(|_| AccessError::Invalid("Runtime Home 无效。"))?;
    if arguments.operation == AccessOperation::Read {
        check_home(&home)?;
        let path = crate::host_layout::read_configuration_path(&home)
            .map_err(|_| AccessError::Unavailable)?;
        return status(&LocalConfigSource::read_only(path), false).await;
    }
    // Read and validate the full bounded request before creating Home or acquiring its existing lock.
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| AccessError::Invalid("无法读取配置请求。"))?;
    if bytes.len() as u64 > MAX_INPUT {
        return Err(AccessError::Invalid("配置请求超过 64 KiB。"));
    }
    let request = match arguments.operation {
        AccessOperation::Configure => {
            let request: Configure = serde_json::from_slice(&bytes)
                .map_err(|_| AccessError::Invalid("访问设置请求无效。"))?;
            validate(&request.configuration)?;
            Change::Configure(request)
        }
        AccessOperation::SetPassword => Change::Password(
            serde_json::from_slice(&bytes).map_err(|_| AccessError::Invalid("密码请求无效。"))?,
        ),
        AccessOperation::Read | AccessOperation::Probe => unreachable!(),
    };
    prepare_runtime_home(&home).map_err(|_| AccessError::Unavailable)?;
    let _instance = RuntimeInstanceGuard::acquire(&home).map_err(|error| match error {
        EndpointError::AlreadyRunning { .. } => AccessError::Busy,
        _ => AccessError::Unavailable,
    })?;
    let original_path =
        crate::host_layout::read_configuration_path(&home).map_err(|_| AccessError::Unavailable)?;
    let original = load(&LocalConfigSource::new(original_path)).await?;
    let expected_revision = match &request {
        Change::Configure(r) => &r.expected_revision,
        Change::Password(r) => &r.expected_revision,
    };
    check_revision(&original, expected_revision)?;
    // Reject invalid requests before the layout upgrade changes the configuration revision.
    let password_hash = match &request {
        Change::Configure(request) => {
            let mut candidate = if home.join(crate::host_configuration::FILE).exists() {
                original.document.clone()
            } else {
                crate::host_configuration::personal_document()
            };
            super::config::configure_identity(
                &mut candidate,
                request.mode,
                request.center_url.as_deref(),
                request.clear_center_binding.unwrap_or(false),
            )?;
            if request.configuration.remote_enabled
                && original.access.password_hash.is_none()
                && crate::host_configuration::parse(&candidate.to_string())
                    .map_err(AccessError::Invalid)?
                    .mode
                    == crate::host_configuration::HostMode::Personal
            {
                return Err(AccessError::Invalid("请先设置密码，再开启非本地访问。"));
            }
            crate::server::tls_configuration(&request.configuration).await?;
            drop(crate::server::bind(request.configuration.port)?);
            None
        }
        Change::Password(request) => Some(
            Credentials::new()
                .hash_password(request.password.clone())
                .await?,
        ),
    };
    let layout_home = home.clone();
    tokio::task::spawn_blocking(move || crate::host_layout::upgrade(&layout_home))
        .await
        .map_err(|_| AccessError::Unavailable)?
        .map_err(|_| AccessError::Unavailable)?;
    let source = LocalConfigSource::new(home.join(crate::host_configuration::FILE));
    let mut document = load(&source).await?;
    match request {
        Change::Configure(request) => {
            super::config::configure_identity(
                &mut document.document,
                request.mode,
                request.center_url.as_deref(),
                request.clear_center_binding.unwrap_or(false),
            )?;
            document.access.public = request.configuration;
        }
        Change::Password(_) => {
            document.access.password_hash = password_hash;
        }
    }
    let saved = save(&source, document).await?;
    let result = status(
        &LocalConfigSource::read_only(home.join(crate::host_configuration::FILE)),
        true,
    )
    .await?;
    if result.revision != saved.revision {
        return Err(AccessError::Conflict);
    }
    Ok(result)
}

enum Change {
    Configure(Configure),
    Password(Password),
}

async fn status(source: &LocalConfigSource, saved: bool) -> Result<Status, AccessError> {
    let document = load(source).await?;
    validate(&document.access.public)?;
    let (mode, center_url, center_id) = super::config::identity_fields(&document.document);
    Ok(Status {
        mode,
        center_url,
        center_id,
        revision: document.revision,
        password_configured: document.access.password_hash.is_some(),
        configuration: document.access.public,
        effective_on_next_start: saved,
    })
}

fn check_home(home: &Path) -> Result<(), AccessError> {
    match std::fs::symlink_metadata(home) {
        Ok(metadata)
            if metadata.is_dir()
                && !crate::platform::is_reparse_or_link(&metadata)
                && crate::platform::owned_by_current_user(home, &metadata)
                && crate::platform::private_directory_mode_is_secured(&metadata) =>
        {
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(AccessError::Invalid(
            "Runtime Home 不是当前用户的私有目录。",
        )),
    }
}

/// 只观察现有内核锁，供没有原生 flock API 的调用方确认停止；不以 PID 代替锁。
fn probe(arguments: &AccessArguments) -> Result<bool, AccessError> {
    use std::{fs, fs::OpenOptions};
    let home = arguments
        .runtime_home()
        .map_err(|_| AccessError::Invalid("Runtime Home 无效。"))?;
    check_home(&home)?;
    let run = home.join("run");
    match fs::symlink_metadata(&run) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Ok(meta)
            if meta.is_dir()
                && !crate::platform::is_reparse_or_link(&meta)
                && crate::platform::owned_by_current_user(&run, &meta)
                && crate::platform::private_directory_mode_is_secured(&meta) => {}
        _ => return Err(AccessError::Unavailable),
    }
    let path = run.join("runtime.lock");
    let mut options = OpenOptions::new();
    options.read(true);
    crate::platform::apply_read_no_follow(&mut options);
    let file = match options.open(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Ok(file) => file,
        Err(_) => return Err(AccessError::Unavailable),
    };
    let opened = file.metadata().map_err(|_| AccessError::Unavailable)?;
    let current = fs::symlink_metadata(&path).map_err(|_| AccessError::Unavailable)?;
    if !opened.is_file()
        || !crate::platform::owned_by_current_user(&path, &opened)
        || !crate::platform::private_file_mode_is_secured(&opened)
        || !crate::platform::same_file_identity(&opened, &current)
    {
        return Err(AccessError::Unavailable);
    }
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(_) => Err(AccessError::Unavailable),
    }
}
