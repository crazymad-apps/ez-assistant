//! 易失 Store 的可靠阅读事实；提交时冻结，不从保留的执行控制重建历史。

use super::*;
use crate::{StoredChildTaskHistory, StoredInputHistory, StoredMessageOrigin, StoredRunHistory};

impl State {
    /// 已消费的选择来自各自 owner 的可靠历史；仅尚在队列中的用户选择来自控制。
    /// 调用方持有 Store 锁，返回只读装配值，不恢复 Input 或模型激活的执行能力。
    pub(super) fn skill_activation_projections(
        &self,
        owner: Option<&SessionId>,
    ) -> Vec<StoredSkillActivation> {
        let mut activations = self
            .skill_activations
            .values()
            .filter(|row| owner.is_none_or(|owner| owner == &row.session_id))
            .cloned()
            .collect::<Vec<_>>();
        for (session_id, history) in &self.histories {
            if owner.is_none_or(|owner| owner == session_id) {
                activations.extend(history.skill_activations.iter().cloned());
            }
        }
        for ((session_id, _), history) in &self.child_histories {
            if owner.is_none_or(|owner| owner == session_id) {
                activations.extend(history.skill_activations.iter().cloned());
            }
        }
        activations.sort_by(|left, right| left.activation_id.cmp(&right.activation_id));
        activations
    }

    pub(super) fn mcp_selection_projections(
        &self,
        owner: Option<&SessionId>,
    ) -> Vec<StoredMcpSelection> {
        let mut selections = self
            .mcp_input_selections
            .values()
            .filter(|row| owner.is_none_or(|owner| owner == &row.session_id))
            .cloned()
            .collect::<Vec<_>>();
        for (session_id, history) in &self.histories {
            if owner.is_none_or(|owner| owner == session_id) {
                selections.extend(history.mcp_selections.iter().cloned());
            }
        }
        selections.sort_by(|left, right| left.selection_id.cmp(&right.selection_id));
        selections
    }

    /// 真实队列仍遵守控制表身份唯一性；历史只在完整 owner 内核对，不把其他会话的同 ID 历史当冲突。
    pub(super) fn contains_skill_activation(
        &self,
        session_id: &SessionId,
        owner: &SkillActivationOwner,
        id: &str,
    ) -> bool {
        self.skill_activations.contains_key(id)
            || match owner {
                SkillActivationOwner::Session(_) => {
                    self.histories.get(session_id).is_some_and(|history| {
                        history
                            .skill_activations
                            .iter()
                            .any(|row| &row.owner == owner && row.activation_id == id)
                    })
                }
                SkillActivationOwner::ChildTask(child_id) => {
                    self.child_histories
                        .iter()
                        .any(|((session, child), history)| {
                            session == session_id
                                && child.as_str() == child_id
                                && history
                                    .skill_activations
                                    .iter()
                                    .any(|row| &row.owner == owner && row.activation_id == id)
                        })
                }
            }
    }

    pub(super) fn contains_mcp_selection(&self, session_id: &SessionId, id: &str) -> bool {
        self.mcp_input_selections.contains_key(id)
            || self.histories.get(session_id).is_some_and(|history| {
                history
                    .mcp_selections
                    .iter()
                    .any(|row| row.selection_id == id)
            })
    }

    /// 在 Store 独占锁内只读合并待执行命令与已冻结结果；不重新安装已完成控制。
    /// 原幂等身份仅用于本地接纳；Fork 命令只有历史事实，不能恢复原 key。
    pub(super) fn command_projection(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> Result<StoredSessionCommand, StoreError> {
        if let Some(control) = self.session_commands.get(input_id)
            && &control.session_id == session_id
        {
            return Ok(control.clone());
        }
        let mut command = self
            .histories
            .get(session_id)
            .and_then(|history| {
                history
                    .commands
                    .iter()
                    .find(|command| &command.input_id == input_id)
            })
            .cloned()
            .ok_or_else(|| conflict("session command history does not exist"))?;
        command.idempotency_key = self
            .command_identities
            .get(input_id)
            .filter(|identity| &identity.session_id == session_id)
            .and_then(|identity| identity.idempotency_key.clone());
        Ok(command)
    }

    /// 只读装配现有端口的命令值；queued 仍来自实际控制，完成项仅来自所属历史。
    pub(super) fn command_projections(
        &self,
        owner: Option<&SessionId>,
    ) -> Result<Vec<StoredSessionCommand>, StoreError> {
        let mut commands = self
            .session_commands
            .values()
            .filter(|command| owner.is_none_or(|owner| owner == &command.session_id))
            .cloned()
            .collect::<Vec<_>>();
        for (session_id, history) in &self.histories {
            if owner.is_some_and(|owner| owner != session_id)
                || self.sessions.get(session_id).is_some_and(|session| {
                    session.conversation_state == StoredConversationState::Unavailable
                })
            {
                continue;
            }
            for command in &history.commands {
                if !self
                    .session_commands
                    .get(&command.input_id)
                    .is_some_and(|control| &control.session_id == session_id)
                {
                    commands.push(self.command_projection(session_id, &command.input_id)?);
                }
            }
        }
        commands.sort_by(|left, right| {
            (left.queue_order, &left.input_id).cmp(&(right.queue_order, &right.input_id))
        });
        Ok(commands)
    }

    /// 仅为读取投影或真正的新 retry 提供原 Input 值；历史字段从可靠事实回读，不安装控制。
    pub(super) fn input_projection(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> Result<StoredInput, StoreError> {
        let identity = self
            .input_identities
            .get(input_id)
            .filter(|identity| &identity.session_id == session_id)
            .ok_or_else(|| conflict("Input acceptance identity is missing"))?;
        if let Some(control) = self
            .inputs
            .get(input_id)
            .filter(|input| &input.session_id == session_id)
        {
            let mut input = control.clone();
            if input.state == StoredInputState::Committed {
                input.skill_activation = self
                    .histories
                    .get(session_id)
                    .and_then(|history| {
                        history
                            .skill_activations
                            .iter()
                            .find(|activation| activation.input_id.as_ref() == Some(input_id))
                    })
                    .cloned();
            }
            return Ok(input);
        }
        let history = self
            .histories
            .get(session_id)
            .ok_or_else(|| conflict("Input history is missing"))?;
        let fact = history
            .inputs
            .iter()
            .find(|input| &input.input_id == input_id && !input.inherited)
            .ok_or_else(|| conflict("local Input history is missing"))?;
        if fact.queue_order != identity.queue_order {
            return Err(StoreError::new(
                StoreErrorKind::InvalidData,
                "Input identity does not match reliable order",
            ));
        }
        Ok(StoredInput {
            input_id: input_id.clone(),
            session_id: session_id.clone(),
            idempotency_key: identity.idempotency_key.clone(),
            queue_order: fact.queue_order,
            agent_variant: fact.agent_variant,
            origin: fact.origin,
            goal_binding: fact.goal_binding.clone(),
            cross_session: fact.cross_session.clone(),
            channel_source: fact.channel_source.clone(),
            agent_shell_target: fact.agent_shell_target,
            skill_activation: history
                .skill_activations
                .iter()
                .find(|activation| activation.input_id.as_ref() == Some(input_id))
                .cloned(),
            user_message_id: fact.user_message_id.clone(),
            state: StoredInputState::Committed,
            queued_message: None,
            accepted_at_ms: fact.accepted_at_ms,
        })
    }

    pub(super) fn input_projections(
        &self,
        owner: Option<&SessionId>,
    ) -> Result<Vec<StoredInput>, StoreError> {
        let mut inputs = Vec::new();
        for (id, identity) in &self.input_identities {
            if owner.is_some_and(|owner| owner != &identity.session_id) {
                continue;
            }
            if !self.inputs.contains_key(id)
                && self
                    .sessions
                    .get(&identity.session_id)
                    .is_some_and(|session| {
                        session.conversation_state == StoredConversationState::Unavailable
                    })
            {
                continue;
            }
            inputs.push(self.input_projection(&identity.session_id, id)?);
        }
        inputs.sort_by(|left, right| {
            (left.queue_order, &left.input_id).cmp(&(right.queue_order, &right.input_id))
        });
        Ok(inputs)
    }

    /// 本次 Run 历史已冻结、其余恢复依赖已闭合；在同一独占提交中释放已完成 Input 载荷。
    /// 当前 Run 紧接着移除，其他真实 attempt 仍依赖此 Input 时保留；最小原始身份不变。
    pub(super) fn release_input_control(
        &mut self,
        input_id: &InputId,
        ending_run: &RunId,
    ) -> Result<(), StoreError> {
        if self
            .runs
            .values()
            .any(|run| &run.input_id == input_id && &run.run_id != ending_run)
        {
            return Ok(());
        }
        let input = self
            .inputs
            .get(input_id)
            .ok_or_else(|| conflict("completed Input control is missing"))?;
        if input.state != StoredInputState::Committed || input.queued_message.is_some() {
            return Err(conflict("unfinished Input control cannot be released"));
        }
        self.input_identities
            .get(input_id)
            .filter(|identity| {
                identity.session_id == input.session_id
                    && identity.idempotency_key == input.idempotency_key
            })
            .ok_or_else(|| conflict("completed Input acceptance identity is missing"))?;
        self.inputs.remove(input_id);
        Ok(())
    }

    pub(super) fn ensure_new_run_identity(
        &self,
        session_id: &SessionId,
        run_id: &RunId,
    ) -> Result<(), StoreError> {
        if self.runs.contains_key(run_id)
            || self
                .histories
                .get(session_id)
                .is_some_and(|history| history.runs.iter().any(|run| &run.run_id == run_id))
        {
            return Err(conflict(
                "new Run identity collides with existing execution or history",
            ));
        }
        Ok(())
    }

    pub(super) fn ensure_new_input_identity(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
        run_id: Option<&RunId>,
        message: &MessageId,
    ) -> Result<(), StoreError> {
        if let Some(run_id) = run_id {
            self.ensure_new_run_identity(session_id, run_id)?;
        }
        let identity_conflict =
            self.input_identities.contains_key(input_id)
                || self.command_identities.contains_key(input_id)
                || self.inputs.contains_key(input_id)
                || self.session_commands.contains_key(input_id)
                || self.materialization_inputs.get(session_id) == Some(input_id)
                || self.inputs.values().any(|input| {
                    &input.session_id == session_id && &input.user_message_id == message
                })
                || self.session_commands.values().any(|command| {
                    &command.session_id == session_id && &command.user_message_id == message
                })
                || self.conversations.get(session_id).is_some_and(|body| {
                    body.messages
                        .iter()
                        .any(|value| message_id(value) == message)
                })
                || self.histories.get(session_id).is_some_and(|history| {
                    history.inputs.iter().any(|input| {
                        &input.input_id == input_id || &input.user_message_id == message
                    }) || history.commands.iter().any(|command| {
                        &command.input_id == input_id || &command.user_message_id == message
                    })
                });
        if identity_conflict {
            return Err(conflict(
                "new Input identity collides with existing execution or history",
            ));
        }
        Ok(())
    }

    /// child 提交在同一 Store 独占锁内冻结自身历史和父目录，再允许释放执行控制。
    /// messages 仅含当前可靠提交的新增正文；不从父 Run 推测 child 的 Run/Input/attempt。
    /// 本函数不获取锁、不运行工具；零消息的 accepted、结算及 generation 更新同样有效。
    pub(super) fn record_child_history(
        &mut self,
        child_task_id: &ChildTaskId,
        messages: &[ConversationMessage],
        step: Option<u32>,
    ) -> Result<(), StoreError> {
        let task = self
            .child_tasks
            .get(child_task_id)
            .ok_or_else(|| conflict("history child control is missing"))?;
        let fact = StoredChildTaskHistory::from_task(task);
        let history = self
            .child_histories
            .entry((task.session_id.clone(), child_task_id.clone()))
            .or_default();
        history.child_tasks.push(fact.clone());
        history
            .message_origins
            .extend(messages.iter().map(|message| StoredMessageOrigin {
                message_id: message_id(message).clone(),
                run_id: None,
                input_id: None,
                attempt: None,
                step,
                inherited: false,
            }));
        history.normalize();
        let directory = self.histories.entry(task.session_id.clone()).or_default();
        directory.child_tasks.push(fact);
        directory.normalize();
        Ok(())
    }

    pub(super) fn child_history(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> Option<&StoredChildTaskHistory> {
        self.child_histories
            .get(&(session_id.clone(), child_task_id.clone()))
            .and_then(|facts| {
                facts
                    .child_tasks
                    .iter()
                    .find(|child| &child.child_task_id == child_task_id)
            })
            .or_else(|| {
                self.histories
                    .get(session_id)?
                    .child_tasks
                    .iter()
                    .find(|child| &child.child_task_id == child_task_id)
            })
    }

    pub(super) fn recovered_child_history(
        &self,
        session_id: Option<&SessionId>,
    ) -> Vec<(SessionId, StoredChildTaskHistory)> {
        let mut children: BTreeMap<_, _> = self
            .histories
            .iter()
            .filter(|(owner, _)| session_id.is_none_or(|id| *owner == id))
            .flat_map(|(owner, history)| {
                history
                    .child_tasks
                    .iter()
                    .cloned()
                    .map(|task| ((owner.clone(), task.child_task_id.clone()), task))
            })
            .collect();
        for (owner, task) in self
            .child_histories
            .iter()
            .filter(|((owner, _), _)| session_id.is_none_or(|id| owner == id))
            .flat_map(|((owner, _), history)| {
                history
                    .child_tasks
                    .iter()
                    .cloned()
                    .map(|task| (owner.clone(), task))
            })
        {
            children
                .entry((owner, task.child_task_id.clone()))
                .or_insert(task);
        }
        children
            .into_iter()
            .map(|((owner, _), task)| (owner, task))
            .collect()
    }

    pub(super) fn session_child_ids(&self, session_id: &SessionId) -> BTreeSet<ChildTaskId> {
        self.child_tasks
            .values()
            .filter(|child| &child.session_id == session_id)
            .map(|child| child.child_task_id.clone())
            .chain(
                self.child_histories
                    .keys()
                    .filter(|(owner, _)| owner == session_id)
                    .map(|(_, id)| id.clone()),
            )
            .chain(
                self.histories
                    .get(session_id)
                    .into_iter()
                    .flat_map(|history| {
                        history
                            .child_tasks
                            .iter()
                            .map(|child| child.child_task_id.clone())
                    }),
            )
            .collect()
    }

    /// 调用方已在 Store 独占锁内完成同次正文/控制变更；冻结后方可释放该 Run 控制。
    /// 不执行任务、不加锁、不生成身份；重试 attempt 不改写先前消息的原始归属。
    pub(super) fn record_run_history(&mut self, run_id: &RunId) -> Result<(), StoreError> {
        let run = self
            .runs
            .get(run_id)
            .ok_or_else(|| conflict("history Run control is missing"))?;
        let input = self
            .inputs
            .get(&run.input_id)
            .ok_or_else(|| conflict("history Input control is missing"))?;
        if run.status == RunStatus::Accepted && run.message_ids.is_empty() {
            return Ok(());
        }
        let mut run_fact = StoredRunHistory::from_run(run, Some(input.user_message_id.clone()));
        if run.status.is_terminal() {
            run_fact.finished_after_message_id = self
                .conversations
                .get(&run.session_id)
                .and_then(|body| body.messages.last())
                .map(|message| message_id(message).clone());
        }
        let history = self.histories.entry(run.session_id.clone()).or_default();
        let mut fact = StoredInputHistory::from_input(input);
        fact.queue_order = self
            .input_identities
            .get(&input.input_id)
            .ok_or_else(|| conflict("Input identity is missing before history commit"))?
            .queue_order;
        history.inputs.push(fact);
        history.runs.push(run_fact);
        for message_id in &run.message_ids {
            if !history
                .message_origins
                .iter()
                .any(|origin| &origin.message_id == message_id)
            {
                history.message_origins.push(StoredMessageOrigin {
                    message_id: message_id.clone(),
                    run_id: Some(run.run_id.clone()),
                    input_id: Some(run.input_id.clone()),
                    attempt: Some(run.attempt),
                    step: run.message_steps.get(message_id).copied(),
                    inherited: false,
                });
            }
        }
        history.skill_activations.extend(
            input
                .skill_activation
                .iter()
                .filter(|activation| run.message_ids.contains(&activation.message_id))
                .cloned(),
        );
        history.mcp_selections.extend(
            self.mcp_input_selections
                .values()
                .filter(|selection| {
                    selection.session_id == run.session_id
                        && run.message_ids.contains(&selection.message_id)
                })
                .cloned(),
        );
        history.normalize();
        // User 与选择已经可靠冻结；同次提交释放此 Input 的临时选择，不影响其他排队原件。
        self.skill_activations
            .retain(|_, activation| activation.input_id.as_ref() != Some(&run.input_id));
        self.mcp_input_selections
            .retain(|_, selection| selection.input_id.as_ref() != Some(&run.input_id));
        self.inputs
            .get_mut(&run.input_id)
            .expect("checked history Input")
            .skill_activation = None;
        Ok(())
    }

    pub(super) fn recovered_run_history(
        &self,
        session_id: Option<&SessionId>,
    ) -> Vec<(SessionId, StoredRunHistory)> {
        self.histories
            .iter()
            .filter(|(owner, _)| session_id.is_none_or(|id| *owner == id))
            .flat_map(|(owner, history)| {
                history.runs.iter().cloned().map(|run| (owner.clone(), run))
            })
            .collect()
    }

    /// 删除影响同时计入可靠历史与真实未完成控制，同一 owner 身份只计一次。
    pub(super) fn session_run_ids(&self, session_id: &SessionId) -> BTreeSet<RunId> {
        self.runs
            .values()
            .filter(|run| &run.session_id == session_id)
            .map(|run| run.run_id.clone())
            .chain(
                self.histories
                    .get(session_id)
                    .into_iter()
                    .flat_map(|history| history.runs.iter().map(|run| run.run_id.clone())),
            )
            .collect()
    }

    /// 重新输入只裁掉原 Input 及尾部历史；其他会话与保留前缀的归属保持原值。
    pub(super) fn retain_rewritten_history(
        &mut self,
        session_id: &SessionId,
        removed: &BTreeSet<InputId>,
        messages: &BTreeSet<MessageId>,
    ) {
        let original_children = self.session_child_ids(session_id);
        let Some(history) = self.histories.get_mut(session_id) else {
            return;
        };
        history
            .inputs
            .retain(|input| !removed.contains(&input.input_id));
        let retained_runs: BTreeSet<_> = history
            .message_origins
            .iter()
            .filter(|origin| messages.contains(&origin.message_id))
            .filter_map(|origin| origin.run_id.as_ref())
            .collect();
        history.runs.retain_mut(|run| {
            if run.input_id.as_ref().is_some_and(|id| removed.contains(id)) {
                return false;
            }
            if run
                .finished_after_message_id
                .as_ref()
                .is_some_and(|id| !messages.contains(id))
            {
                if !retained_runs.contains(&run.run_id) {
                    return false;
                }
                run.partial = true;
                run.status = None;
                run.finished_at_ms = None;
                run.finished_after_message_id = None;
                run.error = None;
            }
            true
        });
        history
            .message_origins
            .retain(|origin| messages.contains(&origin.message_id));
        history
            .commands
            .retain(|command| messages.contains(&command.user_message_id));
        history
            .skill_activations
            .retain(|activation| messages.contains(&activation.message_id));
        history
            .mcp_selections
            .retain(|selection| messages.contains(&selection.message_id));
        history.feedback.retain(|feedback| {
            messages
                .iter()
                .any(|id| id.as_str() == feedback.message_id.as_str())
        });
        let runs: BTreeSet<_> = history.runs.iter().map(|run| &run.run_id).collect();
        history
            .questions
            .retain(|question| runs.contains(&question.run_id));
        history
            .child_tasks
            .retain(|child| runs.contains(&child.parent_run_id));
        let retained_children: BTreeSet<_> = history
            .child_tasks
            .iter()
            .map(|child| child.child_task_id.clone())
            .collect();
        self.child_histories
            .retain(|(owner, id), _| owner != session_id || retained_children.contains(id));
        self.child_conversations.retain(|(owner, id), _| {
            owner != session_id || !original_children.contains(id) || retained_children.contains(id)
        });
    }
}
