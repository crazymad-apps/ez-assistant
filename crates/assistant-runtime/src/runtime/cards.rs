//! 页面实例是当前 Runtime 内的短期权限句柄；快照权威值仍只存在 Store。

use super::AssistantRuntime;
use crate::{
    MessageCardSnapshotWrite, PluginUseLease, RuntimeError, RuntimeResult, StoredMessageCard,
};
use assistant_protocol::*;
use std::time::{Duration, Instant};

pub(super) struct MountedCard {
    owner: ConversationOwner,
    card_id: String,
    client: String,
    identity: String,
    expires: Instant,
}

struct CurrentEntry {
    identity: String,
    entry: Option<PluginUiEntry>,
    lease: Option<PluginUseLease>,
}

impl AssistantRuntime {
    /// 查询当前 owner 已加载轮次的有界卡片摘要，不读页面数据或创建执行。
    /// # Errors
    /// 会话不属于本 Runtime 或查询超限时拒绝。
    pub async fn list_message_cards(
        &self,
        request: ListMessageCardsRequest,
    ) -> RuntimeResult<ListMessageCardsResult> {
        self.ensure_running()?;
        self.session_loader
            .load(crate::message_card_owner(&request.owner).0)
            .await?;
        self.store
            .list_message_cards(request)
            .await
            .map_err(|e| RuntimeError::from_store("list message cards", e))
    }

    async fn current_card_entry(&self, card: &StoredMessageCard) -> RuntimeResult<CurrentEntry> {
        let plugin = match &card.summary.source {
            MessageCardSource::Plugin { name, entry_id, .. } => Some((name, entry_id)),
            MessageCardSource::Mcp {
                target:
                    McpServerTarget {
                        source: McpServerSource::Plugin(name),
                        ..
                    },
                entry_id: Some(id),
                ..
            } => Some((name, id)),
            _ => None,
        };
        if let Some((name, id)) = plugin {
            let (entry, lease, settings_revision) = self.plugin.ui_entry(name, id).await?;
            if entry.kind != PluginUiEntryKind::Card
                || card.snapshot.as_ref().is_some_and(|s| {
                    entry.snapshot_schema.as_ref() != Some(&s.schema)
                        && !entry.readable_snapshot_schemas.contains(&s.schema)
                })
            {
                return Err(unavailable());
            }
            let schema = entry.input_schema.clone();
            let input = card.arguments.clone();
            if !tokio::task::spawn_blocking(move || {
                crate::mcp::compile_schema(&schema).is_ok_and(|v| v.is_valid(&input))
            })
            .await
            .unwrap_or(false)
            {
                return Err(unavailable());
            }
            if let MessageCardSource::Mcp {
                target,
                resource_uri,
                ..
            } = &card.summary.source
                && !matches!(&entry.resource,PluginUiResource::Mcp{server,uri} if server==target.server_key.as_str() && uri==resource_uri)
            {
                return Err(unavailable());
            }
            return Ok(CurrentEntry {
                identity: format!("{}:{settings_revision}:{id}", lease.sha256),
                entry: Some(entry),
                lease: Some(lease),
            });
        }
        let MessageCardSource::Mcp { target, .. } = &card.summary.source else {
            return Err(unavailable());
        };
        let fingerprint = card.source_fingerprint.as_deref().ok_or_else(unavailable)?;
        if !self
            .mcp_service
            .registry
            .ui_source_matches(target, fingerprint)
        {
            return Err(unavailable());
        }
        Ok(CurrentEntry {
            identity: fingerprint.into(),
            entry: None,
            lease: None,
        })
    }

    /// 按当前包/连接重建页面，绑定短期实例并交付最新成功快照；不执行历史工具。
    /// # Errors
    /// 入口不可用、快照不兼容、配置不全或实例超限时拒绝并保留历史值。
    pub async fn mount_message_card(
        &self,
        request: MountMessageCardRequest,
    ) -> RuntimeResult<MountMessageCardResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if request.client_instance_id.is_empty() || request.client_instance_id.len() > 128 {
            return Err(unavailable());
        }
        let session = self
            .session_loader
            .load(crate::message_card_owner(&request.owner).0)
            .await?;
        let card = self
            .store
            .get_message_card(request.owner.clone(), request.card_id.clone())
            .await
            .map_err(|e| RuntimeError::from_store("read message card", e))?
            .ok_or_else(unavailable)?;
        let current = self.current_card_entry(&card).await?;
        let tool_source = match (&current.entry, &current.lease, &card.summary.source) {
            (Some(entry), Some(lease), _) => {
                self.plugin_ui_tool_source(entry, lease, &current.identity)
            }
            (None, None, MessageCardSource::Mcp { target, .. }) => {
                self.mcp_ui_tool_source(target, &current.identity)
            }
            _ => None,
        };
        let resource = if let Some(entry) = &current.entry {
            self.read_plugin_ui_entry_resource(
                entry,
                current.lease.as_ref().ok_or_else(unavailable)?,
            )
            .await?
        } else {
            let MessageCardSource::Mcp {
                target,
                resource_uri,
                ..
            } = &card.summary.source
            else {
                return Err(unavailable());
            };
            self.mcp_service
                .registry
                .read_ui_resource(target, resource_uri, self.root_cancellation.child_token())
                .await?
        };
        // 读取代码不持有 Session gate；返回前重新核对卡片是否被清理，随后替换同客户端旧句柄。
        let _mutation = session.mutation().await;
        let latest = self
            .store
            .get_message_card(request.owner.clone(), request.card_id.clone())
            .await
            .map_err(|e| RuntimeError::from_store("mount message card", e))?
            .ok_or_else(unavailable)?;
        let mut instances = self.card_instances.lock().await;
        let obsolete = instances
            .iter()
            .filter(|(_, i)| {
                i.expires <= Instant::now()
                    || (i.card_id == request.card_id && i.client == request.client_instance_id)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in obsolete {
            instances.remove(&id);
            self.ui_tools.release(&id)?;
        }
        if instances.len() >= 512 {
            return Err(unavailable());
        }
        let id = crate::id::generate("card-instance").map_err(|_| unavailable())?;
        let mut capabilities = current
            .entry
            .as_ref()
            .map(|e| e.capabilities.clone())
            .unwrap_or_default();
        capabilities.retain(|capability| capability != &PluginUiCapability::CallTool);
        if let Some(source) = tool_source
            && session.ensure_active().is_ok()
        {
            let scope = UiAppScope::Session {
                session_id: crate::message_card_owner(&request.owner).0.clone(),
            };
            let source = source.for_card(request.owner.clone(), request.card_id.clone());
            if self.mount_ui_tools(id.clone(), source, scope, latest.summary.title.clone())? {
                capabilities.push(PluginUiCapability::CallTool);
            }
        }
        instances.insert(
            id.clone(),
            MountedCard {
                owner: request.owner,
                card_id: request.card_id,
                client: request.client_instance_id,
                identity: current.identity,
                expires: Instant::now() + Duration::from_secs(3600),
            },
        );
        let writable = current
            .entry
            .as_ref()
            .filter(|e| e.capabilities.contains(&PluginUiCapability::SaveSnapshot))
            .and_then(|e| e.snapshot_schema.clone());
        Ok(MountMessageCardResult {
            card: latest.summary,
            instance_id: id,
            resource,
            arguments: latest.arguments,
            tool_result: latest.tool_result,
            snapshot: latest.snapshot,
            writable_snapshot_schema: writable,
            capabilities,
        })
    }

    /// 撤销指定页面实例；重复释放安全，不改变卡片或外部业务。
    /// # Errors
    /// Runtime 已停止时拒绝。
    pub async fn release_message_card(
        &self,
        request: ReleaseMessageCardRequest,
    ) -> RuntimeResult<()> {
        self.ensure_running()?;
        let mut instances = self.card_instances.lock().await;
        if instances
            .get(&request.instance_id)
            .is_some_and(|i| i.owner == request.owner && i.card_id == request.card_id)
        {
            instances.remove(&request.instance_id);
            self.ui_tools.release(&request.instance_id)?;
        }
        Ok(())
    }

    /// Session gate → 页面实例门 → Store CAS；入口租约仅持有本次保存期间。
    /// 不创建 Run、不写模型正文。过期/替换实例及来源变化均拒绝，冲突返回当前可靠值。
    /// # Errors
    /// 页面、来源或格式失效以及存储失败时拒绝。
    pub async fn save_message_card_snapshot(
        &self,
        request: SaveMessageCardSnapshotRequest,
    ) -> RuntimeResult<SaveMessageCardSnapshotResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if serde_json::to_vec(&request.snapshot.value).map_or(true, |b| b.len() > 256 * 1024) {
            return Err(unavailable());
        }
        let session = self
            .session_loader
            .load(crate::message_card_owner(&request.owner).0)
            .await?;
        let card = self
            .store
            .get_message_card(request.owner.clone(), request.card_id.clone())
            .await
            .map_err(|e| RuntimeError::from_store("read card snapshot", e))?
            .ok_or_else(unavailable)?;
        let current = self.current_card_entry(&card).await?;
        if !current.entry.as_ref().is_some_and(|e| {
            e.capabilities.contains(&PluginUiCapability::SaveSnapshot)
                && e.snapshot_schema.as_ref() == Some(&request.snapshot.schema)
        }) {
            return Err(unavailable());
        }
        let _mutation = session.mutation().await;
        let mut instances = self.card_instances.lock().await;
        let instance = instances
            .get_mut(&request.instance_id)
            .filter(|i| {
                i.owner == request.owner
                    && i.card_id == request.card_id
                    && i.identity == current.identity
                    && i.expires > Instant::now()
            })
            .ok_or_else(unavailable)?;
        instance.expires = Instant::now() + Duration::from_secs(3600);
        let result = self
            .store
            .save_message_card_snapshot(MessageCardSnapshotWrite {
                owner: request.owner.clone(),
                card_id: request.card_id,
                expected_revision: request.expected_revision,
                snapshot: request.snapshot,
            })
            .await
            .map_err(|e| RuntimeError::from_store("save card snapshot", e))?;
        if result.saved {
            self.publish(RuntimeEvent::MessageCardsChanged {
                owner: request.owner,
            });
        }
        Ok(result)
    }
}

fn unavailable() -> RuntimeError {
    RuntimeError::InvalidRequest {
        reason: "卡片页面、入口或快照不可用，请重新加载",
    }
}
