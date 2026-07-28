//! Cumulative offline version scenario registry.

use std::{future::Future, pin::Pin};

use crate::{
    HarnessError,
    cli::VersionBaseline,
    runtime::{HarnessRunId, MessageRole, RunStatus},
};

mod v0_2;

pub(crate) type ScenarioFuture =
    Pin<Box<dyn Future<Output = Result<ScenarioReport, HarnessError>> + Send>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VersionEntry {
    pub(crate) version: &'static str,
    pub(crate) capabilities: &'static [&'static str],
    pub(crate) offline_verify: Option<VersionBaseline>,
    pub(crate) manual_modes: &'static [&'static str],
}

#[derive(Clone, Copy)]
pub(crate) struct ScenarioDefinition {
    pub(crate) name: &'static str,
    pub(crate) baseline: VersionBaseline,
    pub(crate) run: fn() -> ScenarioFuture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScenarioStatus {
    Passed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScenarioReport {
    pub(crate) name: &'static str,
    pub(crate) status: ScenarioStatus,
    pub(crate) run_id: HarnessRunId,
    pub(crate) terminal: RunStatus,
    pub(crate) event_summary: Vec<String>,
    pub(crate) journal_roles: Vec<MessageRole>,
    pub(crate) pending_count: usize,
}

#[derive(Debug)]
pub(crate) struct ScenarioResult {
    pub(crate) name: &'static str,
    pub(crate) status: ScenarioStatus,
    pub(crate) report: Option<ScenarioReport>,
    pub(crate) error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct VerificationSummary {
    pub(crate) baseline: VersionBaseline,
    pub(crate) results: Vec<ScenarioResult>,
}

impl VerificationSummary {
    pub(crate) fn passed(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.status == ScenarioStatus::Passed)
            .count()
    }

    pub(crate) fn failed(&self) -> usize {
        self.results.len() - self.passed()
    }

    pub(crate) fn is_success(&self) -> bool {
        self.failed() == 0
    }
}

const VERSIONS: &[VersionEntry] = &[
    VersionEntry {
        version: "v0.1",
        capabilities: &[
            "OpenAI-compatible Provider single-turn streaming",
            "provider-neutral model events and request projection",
        ],
        offline_verify: None,
        manual_modes: &["chat"],
    },
    VersionEntry {
        version: "v0.2",
        capabilities: &[
            "Agent execution lifecycle and cancellation",
            "tool loop with authorization and two-phase journal",
            "explicit budgets and reliable terminal events",
        ],
        offline_verify: Some(VersionBaseline::V0_2),
        manual_modes: &["chat"],
    },
];

const SCENARIOS: &[ScenarioDefinition] = &[
    ScenarioDefinition {
        name: "plain_text",
        baseline: VersionBaseline::V0_2,
        run: v0_2::plain_text,
    },
    ScenarioDefinition {
        name: "single_tool_loop",
        baseline: VersionBaseline::V0_2,
        run: v0_2::single_tool_loop,
    },
    ScenarioDefinition {
        name: "allow_deny_batch",
        baseline: VersionBaseline::V0_2,
        run: v0_2::allow_deny_batch,
    },
    ScenarioDefinition {
        name: "controlled_failure",
        baseline: VersionBaseline::V0_2,
        run: v0_2::controlled_failure,
    },
    ScenarioDefinition {
        name: "cancelled",
        baseline: VersionBaseline::V0_2,
        run: v0_2::cancelled,
    },
    ScenarioDefinition {
        name: "observation_disconnect",
        baseline: VersionBaseline::V0_2,
        run: v0_2::observation_disconnect,
    },
];

pub(crate) fn versions() -> &'static [VersionEntry] {
    VERSIONS
}

pub(crate) fn definitions(baseline: VersionBaseline) -> Vec<ScenarioDefinition> {
    SCENARIOS
        .iter()
        .copied()
        .filter(|definition| definition.baseline == baseline)
        .collect()
}

pub(crate) async fn verify(baseline: VersionBaseline) -> VerificationSummary {
    run_definitions(baseline, &definitions(baseline)).await
}

async fn run_definitions(
    baseline: VersionBaseline,
    definitions: &[ScenarioDefinition],
) -> VerificationSummary {
    let mut results = Vec::with_capacity(definitions.len());
    for definition in definitions {
        let result = match (definition.run)().await {
            Ok(report) if report.name == definition.name => ScenarioResult {
                name: definition.name,
                status: ScenarioStatus::Passed,
                report: Some(report),
                error: None,
            },
            Ok(report) => ScenarioResult {
                name: definition.name,
                status: ScenarioStatus::Failed,
                report: None,
                error: Some(format!("scenario returned report for `{}`", report.name)),
            },
            Err(error) => ScenarioResult {
                name: definition.name,
                status: ScenarioStatus::Failed,
                report: None,
                error: Some(error.to_string()),
            },
        };
        results.push(result);
    }
    VerificationSummary { baseline, results }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static FOLLOW_UP_RUNS: AtomicUsize = AtomicUsize::new(0);

    fn fails() -> ScenarioFuture {
        Box::pin(async {
            Err(HarnessError::ScenarioFailed(
                "intentional failure".to_owned(),
            ))
        })
    }

    fn follows_failure() -> ScenarioFuture {
        Box::pin(async {
            FOLLOW_UP_RUNS.fetch_add(1, Ordering::Relaxed);
            Err(HarnessError::ScenarioFailed(
                "second intentional failure".to_owned(),
            ))
        })
    }

    #[test]
    fn registry_contains_the_six_v0_2_scenarios() {
        let definitions = definitions(VersionBaseline::V0_2);
        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name)
                .collect::<Vec<_>>(),
            vec![
                "plain_text",
                "single_tool_loop",
                "allow_deny_batch",
                "controlled_failure",
                "cancelled",
                "observation_disconnect",
            ]
        );
        let version = versions()
            .iter()
            .find(|entry| entry.version == "v0.2")
            .expect("v0.2 registry entry");
        assert_eq!(version.offline_verify, Some(VersionBaseline::V0_2));
    }

    #[tokio::test]
    async fn a_failed_scenario_does_not_hide_following_reports() {
        FOLLOW_UP_RUNS.store(0, Ordering::Relaxed);
        let summary = run_definitions(
            VersionBaseline::V0_2,
            &[
                ScenarioDefinition {
                    name: "first",
                    baseline: VersionBaseline::V0_2,
                    run: fails,
                },
                ScenarioDefinition {
                    name: "second",
                    baseline: VersionBaseline::V0_2,
                    run: follows_failure,
                },
            ],
        )
        .await;
        assert_eq!(summary.results.len(), 2);
        assert_eq!(summary.failed(), 2);
        assert_eq!(FOLLOW_UP_RUNS.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn every_registered_v0_2_scenario_passes() {
        let summary = verify(VersionBaseline::V0_2).await;
        let failures = summary
            .results
            .iter()
            .filter_map(|result| result.error.as_deref())
            .collect::<Vec<_>>();
        assert!(summary.is_success(), "scenario failures: {failures:?}");
        assert_eq!(summary.passed(), 6);
    }
}
