//! 文件 I/O 前复核默认权限规则，并按规则过滤目录遍历；底层复用 LocalFileSystem。
use crate::user_paths::UserPaths;
use agent_tools::{
    AbsolutePath, DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult,
    FileSystemTool, FileToolContext, FileToolError, FsFuture, ListDirectoryRequest,
    ListDirectoryResult, ReadFileRequest, ReadFileResult, SearchFilesRequest, SearchFilesResult,
    WriteFileRequest, WriteFileResult,
};
use agent_tools_local::LocalFileSystem;
use std::sync::Arc;

pub(super) struct UserFileSystem {
    pub(super) inner: LocalFileSystem,
    pub(super) paths: Arc<UserPaths>,
}

impl UserFileSystem {
    async fn check(&self, path: &AbsolutePath, write: bool) -> Result<AbsolutePath, FileToolError> {
        let paths = self.paths.clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || paths.resolve(path.as_path(), write))
            .await
            .map_err(|_| FileToolError::io("file path check failed"))?
            .map_err(|_| FileToolError::io("此路径不属于当前用户可访问的资源。"))
            .and_then(|p| {
                AbsolutePath::new(p).map_err(|_| FileToolError::invalid_input("invalid file path"))
            })
    }
}

impl FileSystemTool for UserFileSystem {
    fn read<'a>(
        &'a self,
        request: ReadFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ReadFileResult> {
        Box::pin(async move {
            self.check(&request.path, false).await?;
            self.inner.read(request, context).await
        })
    }
    fn write<'a>(
        &'a self,
        request: WriteFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, WriteFileResult> {
        Box::pin(async move {
            self.check(&request.path, true).await?;
            self.inner.write(request, context).await
        })
    }
    fn edit<'a>(
        &'a self,
        request: EditFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, EditFileResult> {
        Box::pin(async move {
            self.check(&request.path, true).await?;
            self.inner.edit(request, context).await
        })
    }
    fn delete<'a>(
        &'a self,
        request: DeleteFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, DeleteFileResult> {
        Box::pin(async move {
            self.check(&request.path, true).await?;
            self.inner.delete(request, context).await
        })
    }
    fn list<'a>(
        &'a self,
        request: ListDirectoryRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ListDirectoryResult> {
        Box::pin(async move {
            self.check(&request.path, false).await?;
            let mut result = self.inner.list(request, context).await?;
            let paths = self.paths.clone();
            tokio::task::spawn_blocking(move || {
                result
                    .entries
                    .retain(|e| paths.resolve(e.path.as_path(), false).is_ok());
                result
            })
            .await
            .map_err(|_| FileToolError::io("file listing check failed"))
        })
    }
    fn search<'a>(
        &'a self,
        mut request: SearchFilesRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, SearchFilesResult> {
        Box::pin(async move {
            request.path = self.check(&request.path, false).await?;
            let paths = self.paths.clone();
            let root = request.path.clone();
            let exclusions =
                tokio::task::spawn_blocking(move || paths.search_exclusions(root.as_path()))
                    .await
                    .map_err(|_| FileToolError::io("search path check failed"))?
                    .map_err(|_| FileToolError::io("search path check failed"))?;
            self.inner
                .search_excluding(request, context, &exclusions)
                .await
        })
    }
}

#[cfg(test)]
mod tests;
