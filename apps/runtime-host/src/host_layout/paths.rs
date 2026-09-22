//! 只重定位本次确实迁移的托管根；外部目录、正文和已冻结历史均不参与转换。

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Relocation {
    pub(crate) source: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) roots: Vec<PathBuf>,
}

impl Relocation {
    pub(crate) fn path(&self, path: &Path) -> PathBuf {
        if !path.is_absolute() || path.components().any(|p| matches!(p, Component::ParentDir)) {
            return path.to_owned();
        }
        for root in &self.roots {
            if let Ok(suffix) = path.strip_prefix(self.source.join(root)) {
                return self.target.join(root).join(suffix);
            }
        }
        path.to_owned()
    }

    pub(crate) fn text(&self, value: &str) -> String {
        self.path(Path::new(value)).to_string_lossy().into_owned()
    }
}
