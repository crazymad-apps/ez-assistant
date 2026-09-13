//! Windows stdio 命令解析；连接使用最终环境的 PATH，参数转义交给标准库。

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use super::{McpConnectionError, McpConnectionFailureKind, connection_error};

/// 只解析已配置的程序，不执行 Shell 探测或下载。返回绝对路径使 cwd 改动不影响启动目标。
/// Rust Command 对 .cmd/.bat 已有 cmd.exe 包装与参数转义，禁止再拼接原始命令行。
pub(super) fn resolve_program(
    program: &str,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<PathBuf, McpConnectionError> {
    let path = environment
        .iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| OsString::from(value))
        .or_else(|| std::env::var_os("PATH"));
    resolve_path(program, cwd, path.as_deref()).ok_or_else(|| connection_error(
        McpConnectionFailureKind::Connect,
        "MCP Windows executable was not found; configure an existing .exe, .cmd or .bat path, or add its directory to PATH",
    ))
}

fn resolve_path(program: &str, cwd: &Path, search: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let program = Path::new(program);
    let explicit = program.is_absolute() || program.components().count() > 1;
    let roots = if explicit {
        vec![cwd.to_path_buf()]
    } else {
        search
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .filter(|root| !root.as_os_str().is_empty())
            .map(|root| {
                if root.is_absolute() {
                    root
                } else {
                    cwd.join(root)
                }
            })
            .collect()
    };
    let extensions: &[&str] = if program.extension().is_some() {
        &[""]
    } else {
        &["exe", "cmd", "bat"]
    };
    // 显式扩展名保持原样；裸名优先 exe，再查 npm cmd shim，不执行关联应用或任意 PATHEXT。
    for extension in extensions {
        for root in &roots {
            let mut candidate = root.join(program);
            if !extension.is_empty() {
                candidate.set_extension(extension);
            }
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
