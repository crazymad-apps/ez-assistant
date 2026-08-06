//! Runtime library 的显式构造配置。

use std::num::NonZeroUsize;

/// Assistant Runtime 初始化版本所需的最小配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    /// Runtime 实时观察事件通道容量；M4 建立 Event Hub 时使用。
    pub event_capacity: NonZeroUsize,
}

impl RuntimeConfig {
    /// 使用显式的有界事件容量创建配置。
    pub fn new(event_capacity: NonZeroUsize) -> Self {
        Self { event_capacity }
    }
}
