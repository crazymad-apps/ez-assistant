//! 单个 Run 的 Runtime 私有工具装配流水线。

use agent_tools::{MergeToolSetError, Tool, ToolRegistry, ToolSetSnapshot};

/// 一个装配环节产生的独立冻结贡献。
pub(super) struct RunToolContribution {
    tools: ToolSetSnapshot,
}

impl RunToolContribution {
    pub(super) fn frozen(tools: ToolSetSnapshot) -> Self {
        Self { tools }
    }

    pub(super) fn tool<T: Tool>(tool: T) -> Result<Self, agent_tools::RegisterToolError> {
        let mut registry = ToolRegistry::new();
        registry.register(tool)?;
        Ok(Self {
            tools: registry.snapshot(),
        })
    }
}

/// 按注册顺序合并贡献并产出一个不可变 ToolSet。
#[derive(Default)]
pub(super) struct RunToolAssembly {
    contributions: Vec<RunToolContribution>,
}

impl RunToolAssembly {
    pub(super) fn contribute(&mut self, contribution: RunToolContribution) {
        self.contributions.push(contribution);
    }

    pub(super) fn freeze(self) -> Result<ToolSetSnapshot, MergeToolSetError> {
        self.contributions
            .into_iter()
            .try_fold(ToolSetSnapshot::default(), |tools, contribution| {
                tools.try_merge(contribution.tools)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::UpdateGoalTool;

    #[test]
    fn assembly_uses_stable_stage_order_and_deduplicates_shared_snapshots() {
        let base = RunToolContribution::tool(UpdateGoalTool::new(None))
            .expect("base tool")
            .tools;
        let mut assembly = RunToolAssembly::default();
        assembly.contribute(RunToolContribution::frozen(base.clone()));
        assembly.contribute(RunToolContribution::frozen(base));

        let tools = assembly.freeze().expect("assembly");
        let names: Vec<&str> = tools
            .definitions()
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        assert_eq!(names, ["update_goal"]);
    }

    #[test]
    fn assembly_fails_closed_on_same_name_from_distinct_stages() {
        let mut assembly = RunToolAssembly::default();
        assembly
            .contribute(RunToolContribution::tool(UpdateGoalTool::new(None)).expect("first tool"));
        assembly
            .contribute(RunToolContribution::tool(UpdateGoalTool::new(None)).expect("second tool"));

        assert!(matches!(
            assembly.freeze(),
            Err(MergeToolSetError::ConflictingName(name)) if name.as_str() == "update_goal"
        ));
    }
}
