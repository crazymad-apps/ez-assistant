//! 与具体存储和应用编排无关的 Agent 记忆能力。
//!
//! 本 crate 的稳定边界包括 Pinned Memory 领域、冻结快照渲染、Memory Recall 与
//! RecallSource 契约。M0 只建立 crate 边界；具体能力会在后续里程碑逐步实现。

pub mod pinned;
mod property;
pub mod recall;

pub use pinned::{
    PinnedMemoryCategory, PinnedMemoryDraft, PinnedMemoryEntry, PinnedMemoryFuture, PinnedMemoryId,
    PinnedMemoryLimits, PinnedMemoryPatch, PinnedMemorySnapshot, PinnedMemorySnapshotError,
    PinnedMemorySnapshotInput, PinnedMemoryStore, PinnedMemoryStoreError,
    PinnedMemoryValidationError,
};
pub use property::MemoryPropertyValue;
pub use recall::{
    CoordinatedMemoryRecall, CoordinatedMemoryRecallConfig, CoordinatedMemoryRecallConfigError,
    MemoryRecall, MemoryRecallError, MemoryRecallFailure, MemoryRecallFuture, MemoryRecallRequest,
    MemoryRecallResponse, RecallFailureKind, RecallItem, RecallOrigin, RecallReadDirection,
    RecallReadWindow, RecallReferenceReadFuture, RecallReferenceReadRequest, RecallReferenceReader,
    RecallScope, RecallSource, RecallSourceError, RecallSourceFuture, RecallSourceId,
    RecallSourceItem, RecallSourceRequest, RecallSourceResponse,
};
