use super::*;

impl VolatileRuntimeStore {
    pub(super) fn shutdown(&self) -> StoreFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
