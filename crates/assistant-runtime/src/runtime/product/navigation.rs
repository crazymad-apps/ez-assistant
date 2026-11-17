//! 产品缩略查询。正文、可靠候选和活动 Step 分别由现有 owner 持有。

use assistant_protocol::{
    ConversationMessagePreview, ConversationNavigationPage, ConversationOwner,
    GetConversationMessagePreviewRequest, GetConversationMessagePreviewResult,
    ListConversationNavigationRequest, ListConversationNavigationResult, ObservedSnapshot, RunId,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::{
    ConversationNavigationRead, ConversationNavigationRequest, NavigationBoundary,
    NavigationReplyContext, RuntimeError, RuntimeResult, StoreErrorKind,
    StoredConversationNavigation, session::SessionController,
};

use super::{AssistantRuntime, SNAPSHOT_ATTEMPTS, owner_session_id};

#[derive(Serialize, Deserialize)]
struct NavigationCursor {
    owner: ConversationOwner,
    generation: u64,
    navigation_boundary: NavigationBoundary,
}

impl AssistantRuntime {
    /// 读取同一个 owner 的一页可靠消息 ID，游标绑定范围和代次。
    pub async fn list_conversation_navigation(
        &self,
        request: ListConversationNavigationRequest,
    ) -> RuntimeResult<ListConversationNavigationResult> {
        let limit = request.limit.unwrap_or(128) as usize;
        if !(1..=256).contains(&limit) {
            return Err(RuntimeError::InvalidRequest {
                reason: "navigation limit must be between 1 and 256",
            });
        }
        let cursor = request
            .cursor
            .as_deref()
            .map(|cursor| {
                URL_SAFE_NO_PAD
                    .decode(cursor)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<NavigationCursor>(&bytes).ok())
                    .filter(|cursor| cursor.owner == request.owner)
                    .ok_or(RuntimeError::InvalidRequest {
                        reason: "navigation cursor is invalid for this owner",
                    })
            })
            .transpose()?;
        let (generation, sequence, read) = self
            .read_conversation_navigation(
                &request.owner,
                cursor.as_ref().map(|cursor| cursor.generation),
                ConversationNavigationRead::Page {
                    boundary: cursor.map(|cursor| cursor.navigation_boundary),
                    limit,
                },
            )
            .await?;
        let StoredConversationNavigation::Page {
            items,
            previous,
            next,
        } = read
        else {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "conversation navigation page",
            });
        };
        let encode = |navigation_boundary| {
            serde_json::to_vec(&NavigationCursor {
                owner: request.owner.clone(),
                generation,
                navigation_boundary,
            })
            .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "navigation cursor encoder",
            })
        };
        let previous_cursor = previous.map(encode).transpose()?;
        let next_cursor = next.map(encode).transpose()?;
        Ok(ListConversationNavigationResult {
            snapshot: ObservedSnapshot {
                observed_at_ms: crate::runtime::now_ms().ok(),
                observed_sequence: sequence,
                value: ConversationNavigationPage {
                    owner: request.owner,
                    generation,
                    items,
                    previous_cursor,
                    next_cursor,
                },
            },
        })
    }

    /// 只读取仍属于缩略导航的消息，保留空文本与真实读取失败的区别。
    pub async fn get_conversation_message_preview(
        &self,
        request: GetConversationMessagePreviewRequest,
    ) -> RuntimeResult<GetConversationMessagePreviewResult> {
        let id = agent_types::MessageId::new(request.message_id.as_str()).map_err(|_| {
            RuntimeError::InvalidRequest {
                reason: "conversation message id is invalid",
            }
        })?;
        let (generation, sequence, read) = self
            .read_conversation_navigation(
                &request.owner,
                Some(request.generation),
                ConversationNavigationRead::Preview { message_id: id },
            )
            .await?;
        let StoredConversationNavigation::Preview(preview) = read else {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "conversation message preview",
            });
        };
        let (text, truncated) = preview.ok_or(RuntimeError::InvalidRequest {
            reason: "conversation message is not available for navigation preview",
        })?;
        Ok(GetConversationMessagePreviewResult {
            snapshot: ObservedSnapshot {
                observed_at_ms: crate::runtime::now_ms().ok(),
                observed_sequence: sequence,
                value: ConversationMessagePreview {
                    owner: request.owner,
                    generation,
                    message_id: request.message_id,
                    text,
                    truncated,
                },
            },
        })
    }

    async fn read_conversation_navigation(
        &self,
        owner: &ConversationOwner,
        expected_generation: Option<u64>,
        read: ConversationNavigationRead,
    ) -> RuntimeResult<(u64, u64, StoredConversationNavigation)> {
        let session_id = owner_session_id(owner);
        let session = self.session(session_id).await?;
        // 先锁 parent 再解析 child 绑定，最后进入 Store；读取不装配执行窗口。
        let _mutation = session.mutation().await;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let observation = self.event_sender.read_session_view(session_id, None);
            let (generation, child_status) = match owner {
                ConversationOwner::MainSession { .. } => {
                    (session.lock_state()?.body_generation, None)
                }
                ConversationOwner::ChildTask { child_task_id, .. } => {
                    let task = match self.product_child_task(session_id, child_task_id).await {
                        Err(RuntimeError::SnapshotStale) => continue,
                        result => result?,
                    }
                    .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                        session_id: session_id.clone(),
                        child_task_id: child_task_id.clone(),
                    })?;
                    if task.body_unavailable {
                        return Err(RuntimeError::from_store(
                            "read child navigation",
                            crate::StoreError::new(
                                StoreErrorKind::ResourceUnavailable,
                                "child body is unavailable",
                            ),
                        ));
                    }
                    (task.body_generation, task.status)
                }
            };
            if expected_generation.is_some_and(|expected| expected != generation) {
                return Err(RuntimeError::SnapshotStale);
            }
            let active_step = self.navigation_active_step(owner, &session)?;
            let result = match self
                .store
                .read_conversation_navigation(ConversationNavigationRequest {
                    owner: owner.clone(),
                    generation,
                    read: read.clone(),
                    context: NavigationReplyContext {
                        active_step: active_step.clone(),
                        child_status,
                    },
                })
                .await
            {
                Ok(result) => result,
                Err(error)
                    if error.kind() == StoreErrorKind::Conflict
                        && expected_generation.is_none() =>
                {
                    continue;
                }
                Err(error) if error.kind() == StoreErrorKind::Conflict => {
                    return Err(RuntimeError::SnapshotStale);
                }
                Err(error) if error.kind() == StoreErrorKind::InvalidInput => {
                    return Err(RuntimeError::InvalidRequest {
                        reason: "navigation cursor position is invalid",
                    });
                }
                Err(error) => {
                    return Err(RuntimeError::from_store(
                        "read conversation navigation",
                        error,
                    ));
                }
            };
            if let Some((sequence, captured_step)) =
                observation.capture(|| self.navigation_active_step(owner, &session))?
                && captured_step == active_step
            {
                return Ok((generation, sequence, result));
            }
        }
        Err(RuntimeError::SnapshotBusy)
    }

    fn navigation_active_step(
        &self,
        owner: &ConversationOwner,
        session: &SessionController,
    ) -> RuntimeResult<Option<(Option<RunId>, u32)>> {
        Ok(match owner {
            ConversationOwner::MainSession { .. } => {
                let state = session.lock_state()?;
                state.active_run.as_ref().and_then(|active| {
                    let step = state.runs.get(&active.run_id)?.active_step()?;
                    Some((Some(active.run_id.clone()), step))
                })
            }
            ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } => self
                .child_tasks
                .active_step(session_id, child_task_id)?
                .map(|step| (None, step)),
        })
    }
}
