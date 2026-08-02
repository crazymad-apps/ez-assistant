//! 三种引擎 Fake（[`crate::ScriptedTool`]、[`crate::InMemoryRecorder`]、
//! [`crate::ScriptedAuthorizer`]）共享的顺序日志。
//!
//! 引擎行为矩阵需要断言副作用前顺序 `begin(Assistant) → authorize → execute →
//! complete(batch)`：三种 fake 把各自的调用追加到同一个 [`OrderLog`]，测试用
//! 一份有序快照完成断言。日志只记录**调用尝试**（含被注入失败拦截的调用）。

use std::sync::{Arc, Mutex};

/// 顺序日志中的一条记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogEntry {
    /// Recorder 收到 begin pending exchange（含注入失败的尝试）。
    RecordAssistant,
    /// Recorder 收到一次整批 complete（含注入失败的尝试）。
    RecordTool,
    /// 顺序策略链评估一次 resolved invocation。
    PolicyEvaluate {
        /// resolved invocation 的工具名。
        name: String,
        /// 原 resolved batch 位置数。
        batch_size: usize,
    },
    /// Authorizer 收到一次授权请求。
    Authorize {
        /// 过闸的 Tool Call 名称。
        name: String,
        /// 本轮批次大小（同轮全部 Tool Call 数）。
        batch_size: usize,
    },
    /// 工具进入 `execute`。
    ToolExecute {
        /// 被执行的工具名称。
        name: String,
    },
    /// 工具收到取消并完成资源清理。
    ToolCleanup {
        /// 完成清理的工具名称。
        name: String,
    },
}

/// 共享顺序日志；克隆体共享同一份条目。
#[derive(Clone, Default)]
pub struct OrderLog {
    entries: Arc<Mutex<Vec<LogEntry>>>,
}

impl OrderLog {
    /// 创建空日志。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一条记录（fake 侧调用）。
    pub fn push(&self, entry: LogEntry) {
        self.entries
            .lock()
            .expect("order log mutex poisoned")
            .push(entry);
    }

    /// 当前全部条目的有序快照（断言用）。
    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries
            .lock()
            .expect("order log mutex poisoned")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_one_ordered_log() {
        let log = OrderLog::new();
        let clone = log.clone();
        log.push(LogEntry::RecordAssistant);
        clone.push(LogEntry::Authorize {
            name: "read_file".to_owned(),
            batch_size: 2,
        });
        log.push(LogEntry::ToolExecute {
            name: "read_file".to_owned(),
        });
        clone.push(LogEntry::RecordTool);
        assert_eq!(
            log.entries(),
            vec![
                LogEntry::RecordAssistant,
                LogEntry::Authorize {
                    name: "read_file".to_owned(),
                    batch_size: 2,
                },
                LogEntry::ToolExecute {
                    name: "read_file".to_owned(),
                },
                LogEntry::RecordTool,
            ]
        );
    }
}
