//! 稳定设备登记与 Controller PC 输出托管用例。

use assistant_protocol::{
    DeviceId, SetCurrentControllerOutputHostingRequest, SetCurrentControllerOutputHostingResult,
};

use super::{AssistantRuntime, now_ms};
use crate::{
    DeviceLifecycle, DeviceNameChange, DeviceRevocation, DeviceRevocationResult, NewPairedDevice,
    PairedDevice, PcOutputHosting, PcOutputHostingChange, RuntimeError, RuntimeResult,
};

const MAX_DEVICE_DISPLAY_NAME_BYTES: usize = 128;

impl AssistantRuntime {
    pub(crate) fn device_names(
        &self,
    ) -> RuntimeResult<std::collections::HashMap<DeviceId, String>> {
        Ok(self
            .devices
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "device registry",
            })?
            .iter()
            .filter(|(_, device)| device.lifecycle == DeviceLifecycle::Paired)
            .map(|(device_id, device)| (device_id.clone(), device.display_name.clone()))
            .collect())
    }

    pub async fn register_paired_device(
        &self,
        device: NewPairedDevice,
    ) -> RuntimeResult<PairedDevice> {
        let _operation = self.operation_gate.read().await;
        let _device_mutation = self.device_mutation_gate.lock().await;
        self.ensure_running()?;
        validate_display_name(&device.display_name)?;
        let stored = self
            .store
            .register_paired_device(device)
            .await
            .map_err(|source| RuntimeError::from_store("register paired device", source))?;
        self.devices
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "device registry",
            })?
            .insert(stored.device_id.clone(), stored.clone());
        Ok(stored)
    }

    pub async fn rename_paired_device(
        &self,
        device_id: DeviceId,
        display_name: String,
    ) -> RuntimeResult<PairedDevice> {
        let _operation = self.operation_gate.read().await;
        let _device_mutation = self.device_mutation_gate.lock().await;
        self.ensure_running()?;
        validate_display_name(&display_name)?;
        let stored = self
            .store
            .rename_device(DeviceNameChange {
                device_id: device_id.clone(),
                display_name: display_name.clone(),
                changed_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("rename device", source))?;
        self.devices
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "device registry",
            })?
            .insert(device_id.clone(), stored.clone());
        let sessions = self
            .sessions
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for session in sessions {
            let mut state = session.lock_state()?;
            if let Some(hosting) = state.pc_output_hosting.as_mut()
                && hosting.device_id == device_id
            {
                hosting.device_name = display_name.clone();
            }
        }
        Ok(stored)
    }

    pub async fn revoke_paired_device(
        &self,
        device_id: DeviceId,
    ) -> RuntimeResult<DeviceRevocationResult> {
        let _operation = self.operation_gate.read().await;
        let _device_mutation = self.device_mutation_gate.lock().await;
        self.ensure_running()?;
        let result = self
            .store
            .revoke_device(DeviceRevocation {
                device_id: device_id.clone(),
                revoked_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("revoke device", source))?;
        self.devices
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "device registry",
            })?
            .insert(device_id, result.device.clone());
        for session_id in &result.cleared_session_ids {
            let session = self.session(session_id)?;
            session.lock_state()?.pc_output_hosting = None;
            self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
                session_id: session_id.clone(),
            });
        }
        Ok(result)
    }

    pub fn paired_device(&self, device_id: &DeviceId) -> RuntimeResult<Option<PairedDevice>> {
        Ok(self
            .devices
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "device registry",
            })?
            .get(device_id)
            .filter(|device| device.lifecycle == DeviceLifecycle::Paired)
            .cloned())
    }

    /// Returns the durable registration regardless of its current lifecycle.
    ///
    /// Authentication uses this lookup to verify proof of possession before revealing that a
    /// formerly paired credential has been revoked. Business input paths must use
    /// [`Self::paired_device`] instead.
    pub fn registered_device(&self, device_id: &DeviceId) -> RuntimeResult<Option<PairedDevice>> {
        Ok(self
            .devices
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "device registry",
            })?
            .get(device_id)
            .cloned())
    }

    pub fn registered_devices(&self) -> RuntimeResult<Vec<PairedDevice>> {
        Ok(self
            .devices
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "device registry",
            })?
            .values()
            .cloned()
            .collect())
    }

    pub async fn set_current_controller_output_hosting(
        &self,
        request: SetCurrentControllerOutputHostingRequest,
    ) -> RuntimeResult<SetCurrentControllerOutputHostingResult> {
        let _operation = self.operation_gate.read().await;
        let _device_mutation = self.device_mutation_gate.lock().await;
        self.ensure_running()?;
        let controller = self
            .controller_sessions()?
            .into_iter()
            .next()
            .ok_or(RuntimeError::ControllerUnavailable)?;
        let _mutation = controller.mutation().await;
        controller.ensure_active()?;
        let target = request
            .device_id
            .as_ref()
            .map(|device_id| {
                self.paired_device(device_id)?
                    .ok_or(RuntimeError::InvalidRequest {
                        reason: "hosting device is not paired",
                    })
            })
            .transpose()?;
        let changed = self
            .store
            .set_pc_output_hosting(PcOutputHostingChange {
                controller_session_id: controller.id().clone(),
                device_id: request.device_id,
            })
            .await
            .map_err(|source| RuntimeError::from_store("set controller output hosting", source))?;
        if changed {
            controller.lock_state()?.pc_output_hosting = target.map(|device| PcOutputHosting {
                device_id: device.device_id,
                device_name: device.display_name,
            });
            self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
                session_id: controller.id().clone(),
            });
        }
        Ok(SetCurrentControllerOutputHostingResult {
            session: controller.summary()?,
            changed,
        })
    }
}

fn validate_display_name(value: &str) -> RuntimeResult<()> {
    if value.trim().is_empty() || value.len() > MAX_DEVICE_DISPLAY_NAME_BYTES {
        return Err(RuntimeError::InvalidRequest {
            reason: "device display name must be non-empty and within 128 bytes",
        });
    }
    Ok(())
}
