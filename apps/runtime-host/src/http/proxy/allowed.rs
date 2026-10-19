//! 两端 Host 共用准入；中心不读取 Command 正文。
use super::super::commands::{CommandRequest, HostCommand};
use assistant_protocol::{PermissionDocumentScope, RuntimeCommand::*, SessionCommand};
pub(super) fn allowed(request: &CommandRequest) -> bool {
    let HostCommand::Runtime(command) = &request.command else {
        return false;
    };
    match command {
        RegisterWorkspace(_)
        | UpdateWorkspace(_)
        | GetWorkspace(_)
        | ListWorkspaces(_)
        | RemoveWorkspace(_)
        | RestoreWorkspace(_)
        | GetApplicationSnapshot(_)
        | GetSessionView(_)
        | GetChildTaskView(_)
        | ListConversationPage(_)
        | GetConversationPageAroundRun(_)
        | GetConversationPageAroundMessage(_)
        | SearchConversationHistory(_)
        | GetConversationRecallWindow(_)
        | GetToolDetail(_)
        | GetAttachment(_)
        | ListAttachments(_)
        | CreateSession(_)
        | ForkSession(_)
        | PrepareDeleteSession(_)
        | DeleteSession(_)
        | ClearSession(_)
        | CompactSession(_)
        | CancelSessionCompaction(_)
        | ListSessions(_)
        | GetSession(_)
        | GetSessionMaterialization(_)
        | SubmitInput(_)
        | ArchiveSession(_)
        | RestoreSession(_)
        | RenameSession(_)
        | GenerateSessionTitle(_)
        | SetMessageFeedback(_)
        | ReenterFromUserMessage(_)
        | PrioritizeQueuedInput(_)
        | InterruptRun(_)
        | ResumeQueuedInput(_)
        | RejectApprovalAndStopRun(_)
        | CancelQueuedInput(_)
        | ResumeSession(_)
        | RetryRun(_)
        | GetRun(_)
        | ListRuns(_)
        | ListChildTasks(_)
        | GetChildTask(_)
        | CancelChildTask(_)
        | CancelRun(_)
        | ClearWorkPlan(_)
        | StopGoal(_)
        | ResumeGoal(_)
        | ClearGoal(_)
        | ListPendingApprovals(_)
        | DecideApproval(_)
        | ListFeedbackRequests(_)
        | AnswerFeedback(_)
        | CancelFeedback(_)
        | SetSessionModel(_)
        | SetSessionReasoningEffort(_)
        | SetSessionVariant(_)
        | SetSessionApprovalMode(_)
        | ReloadPermissions(_)
        | ListProviders(_)
        | ListProviderModels(_)
        | GetModelSettings(_)
        | GetModelConfiguration(_)
        | ListFixedModelConfigs(_)
        | GetAgentShellSettings { .. }
        | ListCustomAgents { .. }
        | PrepareCustomAgent(_)
        | ListSkills(_)
        | GetSkillDetail(_)
        | ListMcpServerOptions(_) => true,
        SubmitSessionCommand(request) => matches!(
            request.command,
            SessionCommand::AgentShellSwitch { .. }
                | SessionCommand::SkillRefresh
                | SessionCommand::McpRefresh { .. }
        ),
        GetPermissionDocument(request) => !matches!(request.scope, PermissionDocumentScope::Global),
        ReplacePermissionDocument(request) => {
            !matches!(request.scope, PermissionDocumentScope::Global)
        }
        _ => false,
    }
}
