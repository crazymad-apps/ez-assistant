//! 严格权限 JSON schema 及其领域校验。

use std::collections::BTreeSet;

use agent_tools::AbsolutePath;
use assistant_protocol::{AgentVariant, PermissionDiagnosticCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionDocument {
    pub schema_version: u32,
    pub rules: Vec<PermissionRule>,
}

impl PermissionDocument {
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            rules: Vec::new(),
        }
    }

    pub fn parse(content: &[u8]) -> Result<Self, PermissionDocumentError> {
        let document: Self = serde_json::from_slice(content).map_err(|_| {
            PermissionDocumentError::new(
                PermissionDiagnosticCode::InvalidDocument,
                "permission file is not valid strict JSON",
            )
        })?;
        document.validate()?;
        Ok(document)
    }

    pub fn validate(&self) -> Result<(), PermissionDocumentError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(PermissionDocumentError::new(
                PermissionDiagnosticCode::UnsupportedSchema,
                "permission schema version is not supported",
            ));
        }
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            rule.validate()?;
            if !ids.insert(rule.id.as_str()) {
                return Err(PermissionDocumentError::new(
                    PermissionDiagnosticCode::InvalidRule,
                    "permission rule ids must be unique",
                ));
            }
        }
        Ok(())
    }

    /// 追加规则；已有相同 effect、variant 集合和 matcher 时复用原 ID。
    pub fn append_rule(&mut self, rule: PermissionRule) -> Result<String, PermissionDocumentError> {
        self.validate()?;
        rule.validate()?;
        if let Some(existing) = self.rules.iter().find(|existing| {
            existing.effect == rule.effect
                && same_variants(&existing.variants, &rule.variants)
                && existing.matcher == rule.matcher
        }) {
            return Ok(existing.id.clone());
        }
        if self.rules.iter().any(|existing| existing.id == rule.id) {
            return Err(PermissionDocumentError::new(
                PermissionDiagnosticCode::InvalidRule,
                "permission rule id already exists",
            ));
        }
        let id = rule.id.clone();
        self.rules.push(rule);
        Ok(id)
    }

    pub fn render(&self) -> Result<Vec<u8>, PermissionDocumentError> {
        self.validate()?;
        let mut encoded = serde_json::to_vec_pretty(self).map_err(|_| {
            PermissionDocumentError::new(
                PermissionDiagnosticCode::InvalidDocument,
                "permission document could not be encoded",
            )
        })?;
        encoded.push(b'\n');
        Ok(encoded)
    }
}

impl Default for PermissionDocument {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRule {
    pub id: String,
    pub effect: PermissionEffect,
    pub variants: Vec<AgentVariant>,
    pub matcher: PermissionMatcher,
}

impl PermissionRule {
    fn validate(&self) -> Result<(), PermissionDocumentError> {
        if self.id.trim().is_empty() || self.id.len() > 128 {
            return Err(invalid_rule("permission rule id is invalid"));
        }
        if self.variants.is_empty() {
            return Err(invalid_rule("permission rule variants must not be empty"));
        }
        let variants = self.variants.iter().copied().collect::<BTreeSet<_>>();
        if variants.len() != self.variants.len() {
            return Err(invalid_rule("permission rule variants must be unique"));
        }
        self.matcher.validate()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEffect {
    Allow,
    Deny,
    Ask,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionMatcher {
    General(GeneralPermissionMatcher),
    File(FilePermissionMatcher),
    Shell(ShellPermissionMatcher),
}

impl PermissionMatcher {
    fn validate(&self) -> Result<(), PermissionDocumentError> {
        match self {
            Self::General(matcher) => matcher.validate(),
            Self::File(matcher) => matcher.validate(),
            Self::Shell(matcher) => matcher.validate(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneralPermissionMatcher {
    pub tool_name: String,
}

impl GeneralPermissionMatcher {
    fn validate(&self) -> Result<(), PermissionDocumentError> {
        if self.tool_name.trim().is_empty() {
            return Err(invalid_rule("general matcher tool name must not be empty"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilePermissionMatcher {
    pub operation: PermissionFileOperation,
    pub path: String,
    pub path_match: PathMatch,
}

impl FilePermissionMatcher {
    fn validate(&self) -> Result<(), PermissionDocumentError> {
        AbsolutePath::new(&self.path)
            .map(|_| ())
            .map_err(|_| invalid_rule("file matcher path must be an absolute UTF-8 path"))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionFileOperation {
    Read,
    List,
    Find,
    Search,
    Write,
    Edit,
    Delete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathMatch {
    Exact,
    Recursive,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellPermissionMatcher {
    pub command: String,
    pub command_match: CommandMatch,
    pub working_directory: String,
    pub process_mode: PermissionProcessMode,
}

impl ShellPermissionMatcher {
    fn validate(&self) -> Result<(), PermissionDocumentError> {
        if self.command.trim().is_empty() {
            return Err(invalid_rule("shell matcher command must not be empty"));
        }
        AbsolutePath::new(&self.working_directory)
            .map(|_| ())
            .map_err(|_| invalid_rule("shell matcher working directory must be absolute UTF-8"))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandMatch {
    Exact,
    Prefix,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProcessMode {
    Managed,
    Detached,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{message}")]
pub struct PermissionDocumentError {
    code: PermissionDiagnosticCode,
    message: &'static str,
}

impl PermissionDocumentError {
    fn new(code: PermissionDiagnosticCode, message: &'static str) -> Self {
        Self { code, message }
    }

    pub fn code(&self) -> PermissionDiagnosticCode {
        self.code
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

fn invalid_rule(message: &'static str) -> PermissionDocumentError {
    PermissionDocumentError::new(PermissionDiagnosticCode::InvalidRule, message)
}

fn same_variants(left: &[AgentVariant], right: &[AgentVariant]) -> bool {
    left.iter().copied().collect::<BTreeSet<_>>() == right.iter().copied().collect::<BTreeSet<_>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str) -> PermissionRule {
        PermissionRule {
            id: id.to_owned(),
            effect: PermissionEffect::Allow,
            variants: vec![AgentVariant::Build],
            matcher: PermissionMatcher::File(FilePermissionMatcher {
                operation: PermissionFileOperation::Write,
                path: "/tmp/output.txt".to_owned(),
                path_match: PathMatch::Exact,
            }),
        }
    }

    #[test]
    fn strict_document_round_trips_with_stable_formatting() {
        let document = PermissionDocument {
            schema_version: 1,
            rules: vec![rule("rule_01")],
        };
        let encoded = document.render().expect("render document");
        assert_eq!(encoded.last(), Some(&b'\n'));
        assert_eq!(
            PermissionDocument::parse(&encoded).expect("parse"),
            document
        );
    }

    #[test]
    fn unknown_fields_and_invalid_combinations_reject_the_whole_file() {
        let unknown = br#"{"schema_version":1,"rules":[],"extra":true}"#;
        assert_eq!(
            PermissionDocument::parse(unknown)
                .expect_err("unknown field")
                .code(),
            PermissionDiagnosticCode::InvalidDocument
        );

        let duplicate = PermissionDocument {
            schema_version: 1,
            rules: vec![rule("same"), rule("same")],
        };
        assert_eq!(
            duplicate.validate().expect_err("duplicate id").code(),
            PermissionDiagnosticCode::InvalidRule
        );

        let matcher_extra = br#"{
            "schema_version": 1,
            "rules": [{
                "id": "rule",
                "effect": "allow",
                "variants": ["plan"],
                "matcher": {
                    "type": "general",
                    "tool_name": "fixture",
                    "extra": true
                }
            }]
        }"#;
        assert_eq!(
            PermissionDocument::parse(matcher_extra)
                .expect_err("matcher unknown field")
                .code(),
            PermissionDiagnosticCode::InvalidDocument
        );

        let relative = PermissionDocument {
            schema_version: 1,
            rules: vec![PermissionRule {
                id: "relative".to_owned(),
                effect: PermissionEffect::Ask,
                variants: vec![AgentVariant::Plan],
                matcher: PermissionMatcher::Shell(ShellPermissionMatcher {
                    command: "cargo test".to_owned(),
                    command_match: CommandMatch::Exact,
                    working_directory: "relative".to_owned(),
                    process_mode: PermissionProcessMode::Managed,
                }),
            }],
        };
        assert_eq!(
            relative.validate().expect_err("relative workdir").code(),
            PermissionDiagnosticCode::InvalidRule
        );
    }

    #[test]
    fn append_reuses_semantically_identical_rule() {
        let mut document = PermissionDocument::empty();
        assert_eq!(
            document.append_rule(rule("first")).expect("append"),
            "first"
        );
        assert_eq!(
            document.append_rule(rule("second")).expect("dedupe"),
            "first"
        );
        assert_eq!(document.rules.len(), 1);
    }
}
