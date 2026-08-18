//! 与具体数据源无关的按需记忆召回能力。

mod coordinator;
mod source;
mod types;

pub use coordinator::{
    CoordinatedMemoryRecall, CoordinatedMemoryRecallConfig, CoordinatedMemoryRecallConfigError,
};
pub use source::{
    MemoryRecall, MemoryRecallFuture, RecallReferenceReadFuture, RecallReferenceReader,
    RecallSource, RecallSourceError, RecallSourceFuture,
};
pub use types::{
    MemoryRecallError, MemoryRecallFailure, MemoryRecallRequest, MemoryRecallResponse,
    RecallFailureKind, RecallItem, RecallOrigin, RecallReadDirection, RecallReadWindow,
    RecallReferenceReadRequest, RecallScope, RecallSourceId, RecallSourceItem, RecallSourceRequest,
    RecallSourceResponse,
};
