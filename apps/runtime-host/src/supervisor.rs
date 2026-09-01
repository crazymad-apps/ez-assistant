//! Runtime Host 进程级任务所有权、故障分级与受控关闭。
//!
//! 本模块只跟踪 Host 自己启动的长期子系统。Session、Run、Store worker 与 Agent 任务仍由
//! `AssistantRuntime` 及其 Store Adapter 管理，不能在这里复制业务状态或恢复策略。

use std::{collections::BTreeMap, error::Error, future::Future, time::Duration};

use thiserror::Error;
use tokio::{task::JoinSet, time::timeout};
use tokio_util::sync::CancellationToken;

type BoxError = Box<dyn Error + Send + Sync>;

/// 子系统退出后对 Host 进程的影响。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailurePolicy {
    /// Desktop HTTP、Runtime 桥接等主入口异常退出后，整个 Host 必须受控关闭。
    ShutdownHost,
    /// Device Gateway、Speech 等可选能力失败后只记录降级，Desktop/Runtime 主链继续运行。
    #[allow(
        dead_code,
        reason = "生产装配可能暂未启用可降级子系统，仍保留该策略供可选能力接入"
    )]
    Degrade,
}

/// Supervisor 登记的长期子系统元数据，用于在任务退出时恢复名称和故障策略。
#[derive(Clone, Copy, Debug)]
struct SubsystemDescriptor {
    name: &'static str,
    failure_policy: FailurePolicy,
}

/// 长期子系统任务的统一完成值；具体错误在 spawn 时擦除以便同组观察。
struct SubsystemCompletion {
    /// Supervisor 只关心子系统是否正常退出；具体错误类型仍由各子系统自行定义。
    result: Result<(), BoxError>,
}

/// Host 进程内唯一的长期子系统 owner。
pub(crate) struct HostSupervisor {
    /// Host 根取消令牌；取消后会向所有已派生的子系统令牌广播关闭请求。
    shutdown: CancellationToken,
    /// 由 Supervisor 持有并负责回收的长期任务集合，避免子系统脱离进程生命周期。
    tasks: JoinSet<SubsystemCompletion>,
    /// Tokio 完成通知只携带任务 ID，此映射用于还原子系统名称和故障策略。
    descriptors: BTreeMap<tokio::task::Id, SubsystemDescriptor>,
    /// 收到关闭请求后等待子系统自行退出的最长时间。
    shutdown_timeout: Duration,
}

impl HostSupervisor {
    pub(crate) fn new(shutdown_timeout: Duration) -> Self {
        Self {
            shutdown: CancellationToken::new(),
            tasks: JoinSet::new(),
            descriptors: BTreeMap::new(),
            shutdown_timeout,
        }
    }

    /// 返回只表达“请求整个 Host 关闭”的句柄，供信号和 Host Command 使用。
    pub(crate) fn shutdown_handle(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// 登记一个 Host 长期子系统，并向它传入独立 child token。
    ///
    /// connection reader/writer 等短任务不直接登记到进程 Supervisor；它们必须由所属 Gateway 或
    /// transport 子系统自己的有界 task group 管理。这样单连接失败不会被误判为进程级故障。
    pub(crate) fn spawn_subsystem<F, Fut, E>(
        &mut self,
        name: &'static str,
        failure_policy: FailurePolicy,
        build: F,
    ) where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), E>> + Send + 'static,
        E: Error + Send + Sync + 'static,
    {
        // child token 只接收根令牌的取消信号，单个子系统不能反向取消整个 Host。
        let child_shutdown = self.shutdown.child_token();
        let abort = self.tasks.spawn(async move {
            SubsystemCompletion {
                result: build(child_shutdown)
                    .await
                    .map_err(|error| Box::new(error) as BoxError),
            }
        });
        // 在任务完成前登记其元数据；后续 JoinSet 返回任务 ID 时据此完成故障分级。
        self.descriptors.insert(
            abort.id(),
            SubsystemDescriptor {
                name,
                failure_policy,
            },
        );
    }

    /// 观察外部关闭信号和全部长期子系统，随后在 deadline 内回收 Host 任务。
    ///
    /// 返回前只保证 Host task group 已收敛；调用方随后仍必须显式调用 Runtime shutdown，让 Runtime
    /// 先结算自己的 Run，再 flush/join Store。即使本函数返回错误也不能跳过该清理步骤。
    pub(crate) async fn run_until<S, E>(mut self, shutdown_signal: S) -> Result<(), SupervisorError>
    where
        S: Future<Output = Result<(), E>> + Send,
        E: Error + Send + Sync + 'static,
    {
        tokio::pin!(shutdown_signal);
        // 保留触发关闭的首要原因；关闭期间出现的次生错误不能覆盖它。
        let mut primary_error = None;

        loop {
            // Host Command 等内部入口可能直接取消根令牌，无需等待外部信号。
            if self.shutdown.is_cancelled() {
                break;
            }
            // 所有长期任务均已退出时，Host 已失去维持服务的入口，应按异常关闭处理。
            if self.tasks.is_empty() {
                primary_error = Some(SupervisorError::NoCriticalSubsystem);
                self.shutdown.cancel();
                break;
            }

            tokio::select! {
                // 同时观察内部关闭请求、操作系统信号和任一子系统退出。
                () = self.shutdown.cancelled() => break,
                signal = &mut shutdown_signal => {
                    if let Err(source) = signal {
                        primary_error = Some(SupervisorError::ShutdownSignal {
                            source: Box::new(source),
                        });
                    }
                    self.shutdown.cancel();
                    break;
                }
                completion = self.tasks.join_next_with_id() => {
                    let Some(completion) = completion else {
                        continue;
                    };
                    let shutdown_started = self.shutdown.is_cancelled();
                    if let Some(error) = self.observe_completion(completion, shutdown_started) {
                        // 致命子系统失败后广播关闭，让其余兄弟任务进入受控回收阶段。
                        primary_error = Some(error);
                        self.shutdown.cancel();
                        break;
                    }
                }
            }
        }

        let drain_error = self.drain_tasks().await.err();
        match (primary_error, drain_error) {
            (Some(error), _) => Err(error),
            (None, Some(error)) => Err(error),
            (None, None) => Ok(()),
        }
    }

    async fn drain_tasks(&mut self) -> Result<(), SupervisorError> {
        // 即使已经发现错误也继续消费全部完成结果，确保没有长期任务被遗留或静默分离。
        let mut first_error = None;
        let shutdown_timeout = self.shutdown_timeout;
        let drain = async {
            while let Some(completion) = self.tasks.join_next_with_id().await {
                if first_error.is_none() {
                    first_error = self.observe_completion(completion, true);
                } else {
                    let _ = self.observe_completion(completion, true);
                }
            }
        };

        if timeout(shutdown_timeout, drain).await.is_err() {
            // 超过 deadline 后才强制中止；先记录仍在运行的名称，便于进程级诊断。
            let remaining = self
                .descriptors
                .values()
                .map(|descriptor| descriptor.name)
                .collect::<Vec<_>>();
            self.tasks.abort_all();
            while self.tasks.join_next().await.is_some() {}
            self.descriptors.clear();
            return Err(SupervisorError::ShutdownTimedOut { remaining });
        }

        first_error.map_or(Ok(()), Err)
    }

    fn observe_completion(
        &mut self,
        completion: Result<(tokio::task::Id, SubsystemCompletion), tokio::task::JoinError>,
        shutdown_started: bool,
    ) -> Option<SupervisorError> {
        let (id, result) = match completion {
            Ok((id, completion)) => (id, completion.result),
            Err(error) => {
                // JoinError 同样保留任务 ID；若元数据缺失，按关键子系统处理以避免静默降级。
                let id = error.id();
                let descriptor = self.descriptors.remove(&id).unwrap_or(SubsystemDescriptor {
                    name: "unknown_host_subsystem",
                    failure_policy: FailurePolicy::ShutdownHost,
                });
                return self.classify_failure(
                    descriptor,
                    SupervisorError::SubsystemTaskFailed {
                        name: descriptor.name,
                        reason: if error.is_panic() {
                            "task panicked"
                        } else {
                            "task was cancelled outside controlled shutdown"
                        },
                    },
                );
            }
        };
        let descriptor = self.descriptors.remove(&id).unwrap_or(SubsystemDescriptor {
            name: "unknown_host_subsystem",
            failure_policy: FailurePolicy::ShutdownHost,
        });

        match result {
            // 只有关闭已经开始时，子系统正常返回才是预期行为。
            Ok(()) if shutdown_started => None,
            Ok(()) => self.classify_failure(
                descriptor,
                SupervisorError::SubsystemTaskFailed {
                    name: descriptor.name,
                    reason: "task exited before Host shutdown",
                },
            ),
            Err(source) => self.classify_failure(
                descriptor,
                SupervisorError::SubsystemFailed {
                    name: descriptor.name,
                    source,
                },
            ),
        }
    }

    fn classify_failure(
        &self,
        descriptor: SubsystemDescriptor,
        error: SupervisorError,
    ) -> Option<SupervisorError> {
        match descriptor.failure_policy {
            FailurePolicy::ShutdownHost => Some(error),
            FailurePolicy::Degrade => {
                // 具体子系统负责把 ready/unavailable/degraded 投影到自身快照；Supervisor 只输出
                // 脱敏的进程诊断，不在这里保存或复制业务状态。
                eprintln!(
                    "runtime-host: subsystem {} degraded: {error}",
                    descriptor.name
                );
                None
            }
        }
    }
}

/// Host 进程级生命周期错误。
///
/// 该错误只描述长期子系统和受控关闭，不承载 Session/Run 的业务失败。
#[derive(Debug, Error)]
pub(crate) enum SupervisorError {
    /// 操作系统或宿主提供的关闭信号观察失败。
    #[error("Host shutdown signal could not be observed")]
    ShutdownSignal {
        #[source]
        source: BoxError,
    },
    /// 子系统完成了任务，但返回了自身定义的业务外错误。
    #[error("Host subsystem {name} failed: {source}")]
    SubsystemFailed {
        name: &'static str,
        #[source]
        source: BoxError,
    },
    /// Tokio 任务 panic、被意外取消，或在关闭前无错误返回。
    #[error("Host subsystem {name} failed because its {reason}")]
    SubsystemTaskFailed {
        name: &'static str,
        reason: &'static str,
    },
    /// 所有进程级长期入口均已退出，Host 无法继续提供服务。
    #[error("Host has no remaining critical subsystem to keep the process alive")]
    NoCriticalSubsystem,
    /// 子系统未能在受控关闭期限内自行退出。
    #[error("Host shutdown timed out with remaining subsystems: {remaining:?}")]
    ShutdownTimedOut { remaining: Vec<&'static str> },
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, sync::Arc};

    use tokio::sync::{Notify, oneshot};

    use super::*;

    #[derive(Debug, Error)]
    #[error("test subsystem failure")]
    struct TestFailure;

    #[tokio::test]
    async fn requested_shutdown_cancels_and_waits_for_owned_subsystem() {
        let mut supervisor = HostSupervisor::new(Duration::from_secs(1));
        let shutdown = supervisor.shutdown_handle();
        let stopped = Arc::new(Notify::new());
        let stopped_by_task = stopped.clone();
        supervisor.spawn_subsystem(
            "desktop_http",
            FailurePolicy::ShutdownHost,
            move |cancellation| async move {
                cancellation.cancelled().await;
                stopped_by_task.notify_one();
                Ok::<_, Infallible>(())
            },
        );

        shutdown.cancel();
        supervisor
            .run_until(std::future::pending::<Result<(), Infallible>>())
            .await
            .expect("controlled shutdown");
        stopped.notified().await;
    }

    #[tokio::test]
    async fn shutdown_signal_failure_cancels_owned_subsystem_and_is_returned() {
        let mut supervisor = HostSupervisor::new(Duration::from_secs(1));
        let stopped = Arc::new(Notify::new());
        let stopped_by_task = stopped.clone();
        supervisor.spawn_subsystem(
            "desktop_http",
            FailurePolicy::ShutdownHost,
            move |cancellation| async move {
                cancellation.cancelled().await;
                stopped_by_task.notify_one();
                Ok::<_, TestFailure>(())
            },
        );

        let error = supervisor
            .run_until(async { Err(TestFailure) })
            .await
            .expect_err("signal failure must stop Host");
        assert!(matches!(error, SupervisorError::ShutdownSignal { .. }));
        stopped.notified().await;
    }

    #[tokio::test]
    async fn critical_failure_cancels_siblings_and_is_returned() {
        let mut supervisor = HostSupervisor::new(Duration::from_secs(1));
        let sibling_stopped = Arc::new(Notify::new());
        let stopped_by_sibling = sibling_stopped.clone();
        supervisor.spawn_subsystem(
            "desktop_http",
            FailurePolicy::ShutdownHost,
            move |cancellation| async move {
                cancellation.cancelled().await;
                stopped_by_sibling.notify_one();
                Ok::<_, TestFailure>(())
            },
        );
        supervisor.spawn_subsystem(
            "critical_runtime_bridge",
            FailurePolicy::ShutdownHost,
            |_| async { Err(TestFailure) },
        );

        let error = supervisor
            .run_until(std::future::pending::<Result<(), Infallible>>())
            .await
            .expect_err("critical failure must stop Host");
        assert!(matches!(
            error,
            SupervisorError::SubsystemFailed {
                name: "critical_runtime_bridge",
                ..
            }
        ));
        sibling_stopped.notified().await;
    }

    #[tokio::test]
    async fn degradable_failure_does_not_stop_critical_subsystem() {
        let mut supervisor = HostSupervisor::new(Duration::from_secs(1));
        let shutdown = supervisor.shutdown_handle();
        let (degraded, degraded_observed) = oneshot::channel();
        supervisor.spawn_subsystem(
            "desktop_http",
            FailurePolicy::ShutdownHost,
            |cancellation| async move {
                cancellation.cancelled().await;
                Ok::<_, TestFailure>(())
            },
        );
        supervisor.spawn_subsystem(
            "optional_device_gateway",
            FailurePolicy::Degrade,
            |_| async move {
                let _ = degraded.send(());
                Err(TestFailure)
            },
        );

        let run = tokio::spawn(async move {
            supervisor
                .run_until(std::future::pending::<Result<(), Infallible>>())
                .await
        });
        degraded_observed.await.expect("degradable task ran");
        assert!(!shutdown.is_cancelled());
        shutdown.cancel();
        run.await
            .expect("supervisor task")
            .expect("degraded failure keeps Host alive");
    }

    #[tokio::test]
    async fn unexpected_critical_exit_is_a_host_failure() {
        let mut supervisor = HostSupervisor::new(Duration::from_secs(1));
        supervisor.spawn_subsystem("desktop_http", FailurePolicy::ShutdownHost, |_| async {
            Ok::<_, TestFailure>(())
        });

        let error = supervisor
            .run_until(std::future::pending::<Result<(), Infallible>>())
            .await
            .expect_err("unexpected exit must stop Host");
        assert!(matches!(
            error,
            SupervisorError::SubsystemTaskFailed {
                name: "desktop_http",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn shutdown_deadline_aborts_unresponsive_subsystem() {
        let mut supervisor = HostSupervisor::new(Duration::from_millis(10));
        let shutdown = supervisor.shutdown_handle();
        supervisor.spawn_subsystem("desktop_http", FailurePolicy::ShutdownHost, |_| async {
            std::future::pending::<()>().await;
            Ok::<_, TestFailure>(())
        });

        shutdown.cancel();
        let error = supervisor
            .run_until(std::future::pending::<Result<(), Infallible>>())
            .await
            .expect_err("unresponsive task must hit the deadline");
        assert!(matches!(
            error,
            SupervisorError::ShutdownTimedOut { ref remaining }
                if remaining == &["desktop_http"]
        ));
    }

    #[tokio::test]
    async fn critical_task_panic_is_observed() {
        let mut supervisor = HostSupervisor::new(Duration::from_secs(1));
        supervisor.spawn_subsystem("desktop_http", FailurePolicy::ShutdownHost, |_| async {
            panic!("private test panic payload");
            #[allow(unreachable_code)]
            Ok::<_, TestFailure>(())
        });

        let error = supervisor
            .run_until(std::future::pending::<Result<(), Infallible>>())
            .await
            .expect_err("panic must stop Host");
        assert!(matches!(
            error,
            SupervisorError::SubsystemTaskFailed {
                name: "desktop_http",
                reason: "task panicked",
            }
        ));
    }
}
