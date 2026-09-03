//! MCP 独立配置文件的 Host 安全来源。

use std::path::PathBuf;

use assistant_runtime::{
    ConfigSourceFuture, ConfigSourceReplaceFuture, McpConfigSource, RuntimeConfigSource,
};

use crate::config_source::LocalConfigSource;

/// 复用 Host 已有的私有文件读取、CAS 与原子替换实现，但保持 Runtime 的 MCP 窄端口。
pub(crate) struct LocalMcpConfigSource {
    inner: LocalConfigSource,
}

impl LocalMcpConfigSource {
    pub(crate) fn new(runtime_home: PathBuf) -> Self {
        Self {
            inner: LocalConfigSource::new(runtime_home.join("mcp.json")),
        }
    }
}

impl McpConfigSource for LocalMcpConfigSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        self.inner.load()
    }

    fn replace(
        &self,
        expected_revision: Option<String>,
        document: String,
    ) -> ConfigSourceReplaceFuture<'_> {
        self.inner.replace(expected_revision, document)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use assistant_runtime::{ConfigSourceLoad, ConfigSourceReplace};

    use super::*;
    use crate::config_source::prepare_runtime_home;

    #[tokio::test]
    async fn mcp_document_uses_private_atomic_source_and_revision_cas() {
        let temporary = tempfile::tempdir().expect("temporary Runtime Home");
        prepare_runtime_home(temporary.path()).expect("prepare Runtime Home");
        let source = LocalMcpConfigSource::new(temporary.path().to_owned());
        assert!(matches!(source.load().await, ConfigSourceLoad::Missing));

        let first = source
            .replace(None, "{\"mcpServers\":{}}\n".to_owned())
            .await;
        let ConfigSourceReplace::Applied(document) = first else {
            panic!("first write should be applied");
        };
        let path = temporary.path().join("mcp.json");
        let metadata = std::fs::metadata(&path).expect("MCP configuration metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        let conflict = source
            .replace(Some("stale".to_owned()), "{}\n".to_owned())
            .await;
        assert!(matches!(conflict, ConfigSourceReplace::Conflict(_)));
        let loaded = source.load().await;
        let ConfigSourceLoad::Document(loaded) = loaded else {
            panic!("MCP configuration should still exist");
        };
        assert_eq!(loaded.revision(), document.revision());
        assert_eq!(loaded.contents(), "{\"mcpServers\":{}}\n");
    }
}
