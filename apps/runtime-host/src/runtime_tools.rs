//! 随 Host 分发的工具路径；只装配子进程环境，不修改进程或系统环境。

use std::{
    env::{JoinPathsError, join_paths, split_paths},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

const BIN: &str = "runtime-tools/bin";

pub(crate) fn directory() -> Option<PathBuf> {
    directory_for(&std::env::current_exe().ok()?)
}

fn directory_for(executable: &Path) -> Option<PathBuf> {
    let parent = executable.parent()?;
    // Tauri 的 macOS 资源跟随整个 App 移动；Client／开发构建放在 Host 相邻目录。
    if parent.file_name() == Some(OsStr::new("MacOS"))
        && parent.parent()?.file_name() == Some(OsStr::new("Contents"))
    {
        return Some(parent.parent()?.join("Resources").join(BIN));
    }
    let adjacent = parent.join(BIN);
    if adjacent.is_dir() {
        return Some(adjacent);
    }
    // Tauri Linux 的 deb/rpm/AppImage 都使用 usr/bin + usr/lib/<package> 布局。
    #[cfg(target_os = "linux")]
    if parent.file_name() == Some(OsStr::new("bin")) {
        let resources = parent.parent()?.join("lib/ez-assistant-desktop").join(BIN);
        if resources.is_dir() {
            return Some(resources);
        }
    }
    None
}

pub(crate) fn ripgrep() -> OsString {
    directory().map_or_else(
        || OsString::from("rg"), // 直接 cargo 构建仍可显式使用开发环境的 rg。
        |directory| directory.join(ripgrep_name()).into_os_string(),
    )
}

fn ripgrep_name() -> &'static str {
    if cfg!(windows) { "rg.exe" } else { "rg" }
}

pub(crate) fn prepend_path(original: Option<&OsStr>) -> Result<Option<OsString>, JoinPathsError> {
    directory()
        .map(|directory| prepend(&directory, original))
        .transpose()
}

fn prepend(directory: &Path, original: Option<&OsStr>) -> Result<OsString, JoinPathsError> {
    join_paths(
        std::iter::once(directory.to_path_buf()).chain(
            original
                .into_iter()
                .flat_map(split_paths)
                .filter(|path| path != directory),
        ),
    )
}

#[cfg(test)]
mod tests;
