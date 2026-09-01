//! 稳定设备登记与 Controller 输出托管的 SQLite 事务。
//!
//! 本模块只持久化 Runtime 的稳定业务事实：设备身份、设备生命周期，以及 Controller
//! 的 PC 输出托管目标。在线连接、心跳、配对窗口和媒体状态属于 Device Gateway 的
//! 临时状态，不在这里保存。
//!
//! 设备撤销与托管解除必须在同一事务中完成，避免已撤销设备继续成为输出目标。

use assistant_protocol::{DeviceId, SessionId};
use assistant_runtime::{
    DeviceLifecycle, DeviceNameChange, DevicePublicKey, DeviceRevocation, DeviceRevocationResult,
    NewPairedDevice, PairedDevice, PcOutputHostingChange,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, internal_error, invalid_data,
    invalid_data_with_source,
};

impl StorageEngine {
    /// 加载全部稳定设备记录，并校验数据库中的设备生命周期不变量。
    ///
    /// 非法数据不会在读取时被静默修复，以便尽早暴露迁移或写入逻辑造成的数据损坏。
    pub(super) fn load_devices(&self) -> StorageResult<Vec<PairedDevice>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT device_id, display_name, public_key, lifecycle,
                        paired_at_ms, updated_at_ms, revoked_at_ms
                 FROM devices ORDER BY paired_at_ms, device_id",
            )
            .map_err(|source| internal_error("devices could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            })
            .map_err(|source| internal_error("devices could not be read", source))?;
        rows.map(|row| {
            let (
                device_id,
                display_name,
                public_key,
                lifecycle,
                paired_at_ms,
                updated_at_ms,
                revoked_at_ms,
            ) = row.map_err(|source| internal_error("device row could not be read", source))?;
            if display_name.trim().is_empty() {
                return Err(invalid_data("stored device display name is invalid"));
            }
            // lifecycle 与 revoked_at_ms 必须成对成立，否则上层无法可靠判断设备是否可用。
            let lifecycle = match (lifecycle.as_str(), revoked_at_ms) {
                ("paired", None) => DeviceLifecycle::Paired,
                ("revoked", Some(_)) => DeviceLifecycle::Revoked,
                _ => return Err(invalid_data("stored device lifecycle is invalid")),
            };
            Ok(PairedDevice {
                device_id: DeviceId::new(device_id).map_err(|source| {
                    invalid_data_with_source("stored device id is invalid", source)
                })?,
                display_name,
                public_key: DevicePublicKey::from_slice(&public_key)
                    .ok_or_else(|| invalid_data("stored device public key is invalid"))?,
                lifecycle,
                paired_at_ms,
                updated_at_ms,
                revoked_at_ms,
            })
        })
        .collect()
    }

    /// 登记配对完成后的稳定设备身份。
    ///
    /// 只有相同 Device ID、相同公钥的已配对设备可以幂等重放；Device ID 或公钥均不允许
    /// 被重新绑定到另一份身份。
    pub(super) fn register_paired_device(
        &mut self,
        device: NewPairedDevice,
    ) -> StorageResult<PairedDevice> {
        if device.display_name.trim().is_empty() {
            return Err(conflict("device display name is empty"));
        }
        let existing = self
            .connection
            .query_row(
                "SELECT display_name, public_key, lifecycle, paired_at_ms, updated_at_ms, revoked_at_ms
                 FROM devices WHERE device_id = ?1",
                [device.device_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("device identity could not be queried", source))?;
        if let Some((
            display_name,
            public_key,
            lifecycle,
            paired_at_ms,
            updated_at_ms,
            revoked_at_ms,
        )) = existing
        {
            // 配对完成响应可能因重试而重复到达，完全相同的活跃身份应返回原记录。
            if lifecycle == "paired" && public_key.as_slice() == device.public_key.as_bytes() {
                return Ok(PairedDevice {
                    device_id: device.device_id,
                    display_name,
                    public_key: device.public_key,
                    lifecycle: DeviceLifecycle::Paired,
                    paired_at_ms,
                    updated_at_ms,
                    revoked_at_ms,
                });
            }
            return Err(conflict("device identity already exists"));
        }
        // 长期设备公钥全局唯一，防止同一物理身份被另一个 Device ID 再次登记。
        let public_key_exists = self
            .connection
            .query_row(
                "SELECT 1 FROM devices WHERE public_key = ?1",
                [device.public_key.as_bytes().as_slice()],
                |_| Ok(()),
            )
            .optional()
            .map_err(|source| internal_error("device public key could not be queried", source))?
            .is_some();
        if public_key_exists {
            return Err(conflict("device public key already exists"));
        }
        self.connection
            .execute(
                "INSERT INTO devices (
                    device_id, display_name, public_key, lifecycle,
                    paired_at_ms, updated_at_ms, revoked_at_ms
                 ) VALUES (?1, ?2, ?3, 'paired', ?4, ?4, NULL)",
                params![
                    device.device_id.as_str(),
                    device.display_name,
                    device.public_key.as_bytes().as_slice(),
                    device.paired_at_ms,
                ],
            )
            .map_err(|source| database_write_error("device could not be registered", source))?;
        Ok(PairedDevice {
            device_id: device.device_id,
            display_name: device.display_name,
            public_key: device.public_key,
            lifecycle: DeviceLifecycle::Paired,
            paired_at_ms: device.paired_at_ms,
            updated_at_ms: device.paired_at_ms,
            revoked_at_ms: None,
        })
    }

    /// 重命名一台仍处于已配对状态的设备，并返回数据库中的最终记录。
    pub(super) fn rename_device(
        &mut self,
        change: DeviceNameChange,
    ) -> StorageResult<PairedDevice> {
        if change.display_name.trim().is_empty() {
            return Err(conflict("device display name is empty"));
        }
        let changed = self
            .connection
            .execute(
                "UPDATE devices SET display_name = ?1, updated_at_ms = ?2
                 WHERE device_id = ?3 AND lifecycle = 'paired'",
                params![
                    change.display_name,
                    change.changed_at_ms,
                    change.device_id.as_str()
                ],
            )
            .map_err(|source| database_write_error("device could not be renamed", source))?;
        if changed != 1 {
            return Err(conflict("device is not paired"));
        }
        self.load_devices()?
            .into_iter()
            .find(|device| device.device_id == change.device_id)
            .ok_or_else(|| invalid_data("renamed device could not be loaded"))
    }

    /// 原子地撤销设备身份，并解除所有指向该设备的 PC 输出托管关系。
    ///
    /// 重复撤销是幂等操作，但仍会清理残留托管引用。返回的 Session ID 供 Runtime
    /// 更新对应会话投影或通知客户端。
    pub(super) fn revoke_device(
        &mut self,
        change: DeviceRevocation,
    ) -> StorageResult<DeviceRevocationResult> {
        // IMMEDIATE 在读取相关状态前取得写入保留，保证校验、撤销和托管清理基于同一串行视图。
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("device revoke transaction could not start", source)
            })?;
        let lifecycle = transaction
            .query_row(
                "SELECT lifecycle FROM devices WHERE device_id = ?1",
                [change.device_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("device lifecycle could not be queried", source))?
            .ok_or_else(|| conflict("device does not exist"))?;
        // 先记录受影响会话；托管字段清空后将无法从数据库还原这份变更集合。
        let mut cleared_session_ids = Vec::new();
        {
            let mut statement = transaction
                .prepare("SELECT session_id FROM sessions WHERE pc_output_device_id = ?1")
                .map_err(|source| {
                    internal_error("device hosting references could not be queried", source)
                })?;
            let rows = statement
                .query_map([change.device_id.as_str()], |row| row.get::<_, String>(0))
                .map_err(|source| {
                    internal_error("device hosting references could not be read", source)
                })?;
            for row in rows {
                cleared_session_ids.push(
                    SessionId::new(row.map_err(|source| {
                        internal_error("device hosting session row could not be read", source)
                    })?)
                    .map_err(|source| {
                        invalid_data_with_source("stored hosting session id is invalid", source)
                    })?,
                );
            }
        }
        // 即使设备此前已经撤销，也清除可能残留的托管引用，以维持幂等后的最终一致状态。
        transaction
            .execute(
                "UPDATE sessions SET pc_output_device_id = NULL WHERE pc_output_device_id = ?1",
                [change.device_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("device hosting references could not be cleared", source)
            })?;
        let changed = lifecycle == "paired";
        if changed {
            transaction
                .execute(
                    "UPDATE devices SET lifecycle = 'revoked', updated_at_ms = ?1, revoked_at_ms = ?1
                     WHERE device_id = ?2 AND lifecycle = 'paired'",
                    params![change.revoked_at_ms, change.device_id.as_str()],
                )
                .map_err(|source| database_write_error("device could not be revoked", source))?;
        }
        transaction.commit().map_err(|source| {
            database_write_error("device revoke transaction could not commit", source)
        })?;
        let device = self
            .load_devices()?
            .into_iter()
            .find(|device| device.device_id == change.device_id)
            .ok_or_else(|| invalid_data("revoked device could not be loaded"))?;
        Ok(DeviceRevocationResult {
            device,
            cleared_session_ids,
            changed,
        })
    }

    /// 设置当前活跃 Controller 的 PC 输出托管目标。
    ///
    /// 目标设备必须仍处于已配对状态；设置为当前值时返回 `false`，不产生业务变更。
    pub(super) fn set_pc_output_hosting(
        &mut self,
        change: PcOutputHostingChange,
    ) -> StorageResult<bool> {
        // 目标校验和 Session 更新必须处于同一写事务，避免设备在两步之间被并发撤销。
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("controller hosting transaction could not start", source)
            })?;
        let current = transaction
            .query_row(
                "SELECT pc_output_device_id FROM sessions
                 WHERE session_id = ?1 AND role = 'controller' AND lifecycle = 'active'",
                [change.controller_session_id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|source| internal_error("controller hosting could not be queried", source))?
            .ok_or_else(|| conflict("controller session does not exist"))?;
        if current.as_deref() == change.device_id.as_ref().map(DeviceId::as_str) {
            transaction.commit().map_err(|source| {
                database_write_error("controller hosting transaction could not commit", source)
            })?;
            return Ok(false);
        }
        if let Some(device_id) = change.device_id.as_ref() {
            let paired = transaction
                .query_row(
                    "SELECT 1 FROM devices WHERE device_id = ?1 AND lifecycle = 'paired'",
                    [device_id.as_str()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|source| internal_error("hosting device could not be queried", source))?
                .is_some();
            if !paired {
                return Err(conflict("hosting device is not paired"));
            }
        }
        let changed = transaction
            .execute(
                "UPDATE sessions SET pc_output_device_id = ?1
                 WHERE session_id = ?2 AND role = 'controller' AND lifecycle = 'active'",
                params![
                    change.device_id.as_ref().map(DeviceId::as_str),
                    change.controller_session_id.as_str(),
                ],
            )
            .map_err(|source| {
                database_write_error("controller hosting could not be changed", source)
            })?;
        if changed != 1 {
            return Err(conflict("controller hosting could not be changed"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("controller hosting transaction could not commit", source)
        })?;
        Ok(true)
    }
}
