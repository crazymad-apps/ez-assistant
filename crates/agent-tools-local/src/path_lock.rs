//! 同一 Adapter 实例内按绝对逻辑路径协调 mutation 的锁表。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use agent_tools::AbsolutePath;
use tokio::sync::Mutex as AsyncMutex;

/// 只在取得或创建异步锁句柄时短暂持有同步锁，绝不跨 `.await`。
#[derive(Default)]
pub(crate) struct PathLockTable {
    locks: Mutex<HashMap<AbsolutePath, Weak<AsyncMutex<()>>>>,
}

impl PathLockTable {
    /// 返回目标逻辑路径共享的异步互斥锁，并顺手清理已经失效的弱引用。
    pub(crate) fn lock_for(&self, path: &AbsolutePath) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(path.clone(), Arc::downgrade(&lock));
        lock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> AbsolutePath {
        AbsolutePath::new(std::env::temp_dir().join(name)).expect("absolute temp path")
    }

    #[test]
    fn same_path_reuses_lock_and_different_paths_do_not() {
        let table = PathLockTable::default();
        let first = table.lock_for(&path("same"));
        let second = table.lock_for(&path("same"));
        let other = table.lock_for(&path("other"));
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other));
    }
}
