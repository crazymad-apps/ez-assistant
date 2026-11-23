//! 会话右栏的偏移和容量约束。各查询先按自己的规则过滤排序，再取页。

use assistant_protocol::{
    ContextPage, ContextPageRequest, ListArtifactsRequest, ListArtifactsResult,
};

use super::AssistantRuntime;
use crate::{RuntimeError, RuntimeResult};

pub(super) struct PageWindow {
    pub(super) offset: u32,
    pub(super) limit: u32,
}

impl PageWindow {
    pub(super) fn new(request: Option<ContextPageRequest>) -> RuntimeResult<Self> {
        let request = request.unwrap_or_default();
        let limit = request.limit.unwrap_or(10);
        if !(1..=50).contains(&limit) {
            return Err(RuntimeError::InvalidRequest {
                reason: "context page limit must be between 1 and 50",
            });
        }
        Ok(Self {
            offset: request.offset.unwrap_or(0),
            limit,
        })
    }

    pub(super) fn take<T>(&self, items: impl ExactSizeIterator<Item = T>) -> (Vec<T>, ContextPage) {
        self.take_counted(items.len(), items)
    }

    pub(super) fn take_counted<T>(
        &self,
        total: usize,
        items: impl Iterator<Item = T>,
    ) -> (Vec<T>, ContextPage) {
        let page = ContextPage {
            offset: self.offset,
            limit: self.limit,
            total: u32::try_from(total).unwrap_or(u32::MAX),
        };
        (
            items
                .skip(self.offset as usize)
                .take(self.limit as usize)
                .collect(),
            page,
        )
    }
}

impl AssistantRuntime {
    /// 直接读取规范产物净状态，沿用原声明顺序；这里只分页摘要，打开时再校验文件。
    pub async fn list_artifacts(
        &self,
        request: ListArtifactsRequest,
    ) -> RuntimeResult<ListArtifactsResult> {
        let window = PageWindow::new(request.page)?;
        let session = self.session(&request.session_id).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let state = session.lock_state()?;
        let journal = state
            .journal
            .as_ref()
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "session artifacts",
            })?;
        let projection = journal.artifacts();
        let (artifacts, page) = window.take_counted(projection.len(), projection.entries());
        let artifacts = artifacts.into_iter().cloned().collect();
        Ok(ListArtifactsResult { artifacts, page })
    }
}
