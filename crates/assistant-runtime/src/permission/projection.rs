//! 权限领域模型与应用协议 DTO 的显式转换。

use assistant_protocol as protocol;

use super::{
    CommandMatch, FilePermissionMatcher, GeneralPermissionMatcher, PathMatch, PermissionDocument,
    PermissionEffect, PermissionFileOperation, PermissionFileRevision, PermissionFileScope,
    PermissionMatcher, PermissionProcessMode, PermissionRule, ShellPermissionMatcher,
    registry::CompiledPermissionLoad,
};

pub(crate) fn scope_from_protocol(scope: protocol::PermissionDocumentScope) -> PermissionFileScope {
    match scope {
        protocol::PermissionDocumentScope::Global => PermissionFileScope::Global,
        protocol::PermissionDocumentScope::Workspace { workspace_id } => {
            PermissionFileScope::Workspace(workspace_id)
        }
        protocol::PermissionDocumentScope::Session { session_id } => {
            PermissionFileScope::Session(session_id)
        }
    }
}

pub(crate) fn scope_to_protocol(scope: &PermissionFileScope) -> protocol::PermissionDocumentScope {
    match scope {
        PermissionFileScope::Global => protocol::PermissionDocumentScope::Global,
        PermissionFileScope::Workspace(workspace_id) => {
            protocol::PermissionDocumentScope::Workspace {
                workspace_id: workspace_id.clone(),
            }
        }
        PermissionFileScope::Session(session_id) => protocol::PermissionDocumentScope::Session {
            session_id: session_id.clone(),
        },
    }
}

pub(crate) fn revision_from_protocol(
    revision: protocol::PermissionDocumentRevision,
) -> PermissionFileRevision {
    match revision {
        protocol::PermissionDocumentRevision::Missing => PermissionFileRevision::Missing,
        protocol::PermissionDocumentRevision::Content { value } => {
            PermissionFileRevision::Content(value)
        }
    }
}

fn revision_to_protocol(revision: &PermissionFileRevision) -> protocol::PermissionDocumentRevision {
    match revision {
        PermissionFileRevision::Missing => protocol::PermissionDocumentRevision::Missing,
        PermissionFileRevision::Content(value) => protocol::PermissionDocumentRevision::Content {
            value: value.clone(),
        },
    }
}

pub(crate) fn document_from_protocol(
    document: protocol::PermissionDocumentDraft,
) -> PermissionDocument {
    PermissionDocument {
        schema_version: document.schema_version,
        rules: document.rules.into_iter().map(rule_from_protocol).collect(),
    }
}

pub(crate) fn snapshot_from_load(
    load: &CompiledPermissionLoad,
) -> protocol::PermissionDocumentSnapshot {
    let (schema_version, rules) = load
        .document
        .as_ref()
        .map(|document| {
            (
                document.schema_version,
                document.rules.iter().map(rule_to_protocol).collect(),
            )
        })
        .unwrap_or((1, Vec::new()));
    protocol::PermissionDocumentSnapshot {
        scope: scope_to_protocol(&load.scope),
        revision: revision_to_protocol(&load.revision),
        status: load.status,
        schema_version,
        rules,
        diagnostics: load.diagnostics.clone(),
        editable: !matches!(load.scope, PermissionFileScope::Global),
    }
}

fn rule_from_protocol(rule: protocol::PermissionRuleDefinition) -> PermissionRule {
    PermissionRule {
        id: rule.id,
        effect: match rule.effect {
            protocol::PermissionRuleEffect::Allow => PermissionEffect::Allow,
            protocol::PermissionRuleEffect::Deny => PermissionEffect::Deny,
            protocol::PermissionRuleEffect::Ask => PermissionEffect::Ask,
        },
        variants: rule.variants,
        matcher: match rule.matcher {
            protocol::PermissionRuleMatcher::General(matcher) => {
                PermissionMatcher::General(GeneralPermissionMatcher {
                    tool_name: matcher.tool_name,
                })
            }
            protocol::PermissionRuleMatcher::File(matcher) => {
                PermissionMatcher::File(FilePermissionMatcher {
                    operation: match matcher.operation {
                        protocol::PermissionFileOperationDefinition::Read => {
                            PermissionFileOperation::Read
                        }
                        protocol::PermissionFileOperationDefinition::List => {
                            PermissionFileOperation::List
                        }
                        protocol::PermissionFileOperationDefinition::Find => {
                            PermissionFileOperation::Find
                        }
                        protocol::PermissionFileOperationDefinition::Search => {
                            PermissionFileOperation::Search
                        }
                        protocol::PermissionFileOperationDefinition::Write => {
                            PermissionFileOperation::Write
                        }
                        protocol::PermissionFileOperationDefinition::Edit => {
                            PermissionFileOperation::Edit
                        }
                        protocol::PermissionFileOperationDefinition::Delete => {
                            PermissionFileOperation::Delete
                        }
                    },
                    path: matcher.path,
                    path_match: match matcher.path_match {
                        protocol::PermissionPathMatch::Exact => PathMatch::Exact,
                        protocol::PermissionPathMatch::Recursive => PathMatch::Recursive,
                    },
                })
            }
            protocol::PermissionRuleMatcher::Shell(matcher) => {
                PermissionMatcher::Shell(ShellPermissionMatcher {
                    command: matcher.command,
                    command_match: match matcher.command_match {
                        protocol::PermissionCommandMatch::Exact => CommandMatch::Exact,
                        protocol::PermissionCommandMatch::Prefix => CommandMatch::Prefix,
                    },
                    working_directory: matcher.working_directory,
                    process_mode: match matcher.process_mode {
                        protocol::PermissionProcessModeDefinition::Managed => {
                            PermissionProcessMode::Managed
                        }
                        protocol::PermissionProcessModeDefinition::Detached => {
                            PermissionProcessMode::Detached
                        }
                    },
                })
            }
        },
    }
}

fn rule_to_protocol(rule: &PermissionRule) -> protocol::PermissionRuleDefinition {
    protocol::PermissionRuleDefinition {
        id: rule.id.clone(),
        effect: match rule.effect {
            PermissionEffect::Allow => protocol::PermissionRuleEffect::Allow,
            PermissionEffect::Deny => protocol::PermissionRuleEffect::Deny,
            PermissionEffect::Ask => protocol::PermissionRuleEffect::Ask,
        },
        variants: rule.variants.clone(),
        matcher: match &rule.matcher {
            PermissionMatcher::General(matcher) => {
                protocol::PermissionRuleMatcher::General(protocol::PermissionGeneralMatcher {
                    tool_name: matcher.tool_name.clone(),
                })
            }
            PermissionMatcher::File(matcher) => {
                protocol::PermissionRuleMatcher::File(protocol::PermissionFileMatcher {
                    operation: match matcher.operation {
                        PermissionFileOperation::Read => {
                            protocol::PermissionFileOperationDefinition::Read
                        }
                        PermissionFileOperation::List => {
                            protocol::PermissionFileOperationDefinition::List
                        }
                        PermissionFileOperation::Find => {
                            protocol::PermissionFileOperationDefinition::Find
                        }
                        PermissionFileOperation::Search => {
                            protocol::PermissionFileOperationDefinition::Search
                        }
                        PermissionFileOperation::Write => {
                            protocol::PermissionFileOperationDefinition::Write
                        }
                        PermissionFileOperation::Edit => {
                            protocol::PermissionFileOperationDefinition::Edit
                        }
                        PermissionFileOperation::Delete => {
                            protocol::PermissionFileOperationDefinition::Delete
                        }
                    },
                    path: matcher.path.clone(),
                    path_match: match matcher.path_match {
                        PathMatch::Exact => protocol::PermissionPathMatch::Exact,
                        PathMatch::Recursive => protocol::PermissionPathMatch::Recursive,
                    },
                })
            }
            PermissionMatcher::Shell(matcher) => {
                protocol::PermissionRuleMatcher::Shell(protocol::PermissionShellMatcher {
                    command: matcher.command.clone(),
                    command_match: match matcher.command_match {
                        CommandMatch::Exact => protocol::PermissionCommandMatch::Exact,
                        CommandMatch::Prefix => protocol::PermissionCommandMatch::Prefix,
                    },
                    working_directory: matcher.working_directory.clone(),
                    process_mode: match matcher.process_mode {
                        PermissionProcessMode::Managed => {
                            protocol::PermissionProcessModeDefinition::Managed
                        }
                        PermissionProcessMode::Detached => {
                            protocol::PermissionProcessModeDefinition::Detached
                        }
                    },
                })
            }
        },
    }
}
