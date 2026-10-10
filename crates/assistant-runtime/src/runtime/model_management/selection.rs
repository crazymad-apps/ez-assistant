use super::*;

enum SelectionPurpose {
    Default,
    AuxiliaryVision,
}

impl AssistantRuntime {
    /// 新选择优先读取已保存模型，无固定记录时由在线目录确认身份；不写 TOML。
    pub async fn set_default_model(
        &self,
        request: assistant_protocol::SetDefaultModelRequest,
    ) -> RuntimeResult<ModelSettings> {
        self.save_model_selection(request.selection, SelectionPurpose::Default)
            .await
    }

    /// 清除辅助选择只修改数据库引用，保留 TOML 中的辅助调用预算。
    pub async fn set_auxiliary_vision_model(
        &self,
        request: assistant_protocol::SetAuxiliaryVisionModelRequest,
    ) -> RuntimeResult<ModelSettings> {
        self.save_model_selection(request.selection, SelectionPurpose::AuxiliaryVision)
            .await
    }

    /// 网络准备不持写门；提交时再次核对 Provider Arc，防止连接或固定值已改变。
    /// 短提交只替换本次用途的引用，保留并发提交的另一用途；落库完成后才更新快照和发事件。
    async fn save_model_selection(
        &self,
        selection: Option<ModelSelection>,
        purpose: SelectionPurpose,
    ) -> RuntimeResult<ModelSettings> {
        self.ensure_model_editable()?;
        self.ensure_running()?;
        let captured = if let Some(selection) = &selection {
            validate_selection(selection)?;
            let snapshot = self.config_registry.snapshot()?;
            let prepared = self
                .config_registry
                .prepare_model(&snapshot, Some(selection), self.store.as_ref())
                .await?;
            if matches!(purpose, SelectionPurpose::AuxiliaryVision)
                && !prepared.model.capabilities().image_input
            {
                return Err(invalid("辅助识图模型必须明确支持图片输入。"));
            }
            Some(prepared)
        } else {
            None
        };
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        if let Some(prepared) = captured {
            prepared.ensure_current(&self.config_registry)?;
        }
        let mut settings = self.managed_models()?.settings.clone();
        let target = match purpose {
            SelectionPurpose::Default => &mut settings.default_model,
            SelectionPurpose::AuxiliaryVision => &mut settings.vision_model,
        };
        if *target == selection {
            return Ok(settings);
        }
        *target = selection;
        let server_history_key = if matches!(purpose, SelectionPurpose::Default) {
            Some(
                crate::id::generate("history")
                    .map_err(|_| invalid("默认模型续接绑定无法创建。"))?,
            )
        } else {
            None
        };
        self.store
            .save_model_settings(settings.clone(), server_history_key.clone())
            .await
            .map_err(|e| RuntimeError::from_store("save model selection", e))?;
        self.managed_models_mut()?.settings = settings.clone();
        // 写门阻止新 Run 编译；当前 Run 保留冻结配置，后续执行读取已提交的新周期。
        if let Some(key) = server_history_key {
            for session in self
                .sessions
                .read()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "sessions",
                })?
                .values()
            {
                let mut state = session.lock_state()?;
                if state.model_selection.is_none() {
                    state.server_history_key = Some(key.clone());
                }
            }
        }
        self.publish(RuntimeEvent::ConfigChanged);
        Ok(settings)
    }
}
