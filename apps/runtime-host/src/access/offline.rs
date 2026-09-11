//! Host-owned local management IPC. No Runtime, Store, discovery or background service is started.

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
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Password {
    expected_revision: Option<String>,
    password: SecretValue,
}
#[derive(Serialize)]
struct Status {
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
                AccessError::Unavailable => "configuration_unavailable",
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
        return status(
            &LocalConfigSource::read_only(home.join("config.toml")),
            false,
        )
        .await;
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
    let source = LocalConfigSource::new(home.join("config.toml"));
    let mut document = load(&source).await?;
    match request {
        Change::Configure(request) => {
            check_revision(&document, &request.expected_revision)?;
            if request.configuration.remote_enabled && document.access.password_hash.is_none() {
                return Err(AccessError::Invalid("请先设置密码，再开启非本地访问。"));
            }
            crate::server::tls_configuration(&request.configuration).await?;
            drop(crate::server::bind(request.configuration.port)?);
            document.access.public = request.configuration;
        }
        Change::Password(request) => {
            check_revision(&document, &request.expected_revision)?;
            document.access.password_hash =
                Some(Credentials::new().hash_password(request.password).await?);
        }
    }
    let saved = save(&source, document).await?;
    let result = status(
        &LocalConfigSource::read_only(home.join("config.toml")),
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
    Ok(Status {
        revision: document.revision,
        password_configured: document.access.password_hash.is_some(),
        configuration: document.access.public,
        effective_on_next_start: saved,
    })
}

fn check_home(home: &Path) -> Result<(), AccessError> {
    use std::os::unix::fs::MetadataExt as _;
    match std::fs::symlink_metadata(home) {
        Ok(metadata)
            if metadata.is_dir()
                && metadata.uid() == nix::unistd::geteuid().as_raw()
                && metadata.mode() & 0o077 == 0 =>
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
    use std::{
        fs,
        fs::OpenOptions,
        os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    };
    let home = arguments
        .runtime_home()
        .map_err(|_| AccessError::Invalid("Runtime Home 无效。"))?;
    check_home(&home)?;
    let run = home.join("run");
    match fs::symlink_metadata(&run) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Ok(meta)
            if meta.is_dir()
                && meta.uid() == nix::unistd::geteuid().as_raw()
                && meta.mode() & 0o077 == 0 => {}
        _ => return Err(AccessError::Unavailable),
    }
    let path = run.join("runtime.lock");
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Ok(file) => file,
        Err(_) => return Err(AccessError::Unavailable),
    };
    let opened = file.metadata().map_err(|_| AccessError::Unavailable)?;
    let current = fs::symlink_metadata(&path).map_err(|_| AccessError::Unavailable)?;
    if !opened.is_file()
        || opened.uid() != nix::unistd::geteuid().as_raw()
        || opened.mode() & 0o077 != 0
        || opened.dev() != current.dev()
        || opened.ino() != current.ino()
    {
        return Err(AccessError::Unavailable);
    }
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(_) => Err(AccessError::Unavailable),
    }
}
