use super::*;
use crate::skill::{ReadSkillReferenceTool, SkillReferenceFuture, SkillReferenceRequest};
use std::sync::{Arc, Mutex};

enum Response {
    Content(Option<String>),
    Failed,
    Pending,
}

struct Source {
    response: Response,
    requests: Mutex<Vec<SkillReferenceRequest>>,
}

impl SkillPackageSource for Source {
    fn scan(&self, _: SkillScanRequest) -> SkillScanFuture<'_> {
        panic!("reference reads must use the frozen catalog, never rescan")
    }
    fn read_reference(&self, request: SkillReferenceRequest) -> SkillReferenceFuture<'_> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            match &self.response {
                Response::Content(content) => Ok(content.clone()),
                Response::Failed => Err(SkillPackageSourceError::new("private source failure")),
                Response::Pending => std::future::pending().await,
            }
        })
    }
}

fn catalog(source: SkillSource) -> SkillCatalog {
    catalog_with(candidate(
        BUILTIN_GUIDE_SKILL_NAME,
        source,
        "builtin:ez-assistant-guide",
    ))
}

fn source(response: Response) -> Arc<Source> {
    Arc::new(Source {
        response,
        requests: Mutex::new(Vec::new()),
    })
}

#[test]
fn reference_tool_is_only_installed_for_a_model_invocable_builtin_winner() {
    let source = source(Response::Content(Some("page".to_owned())));
    assert!(
        ReadSkillReferenceTool::for_catalog(&catalog(SkillSource::Builtin), source.clone())
            .is_some()
    );
    for source_kind in [
        SkillSource::WorkspaceEzAssistant,
        SkillSource::UserEzAssistant,
        SkillSource::Plugin,
    ] {
        // 模拟已归并的自定义 Winner，不能绕过它退回内置主题。
        let mut winner = catalog(SkillSource::Builtin);
        winner.definitions[0].source = source_kind;
        assert!(ReadSkillReferenceTool::for_catalog(&winner, source.clone()).is_none());
    }
    let mut hidden = catalog(SkillSource::Builtin);
    hidden.definitions[0].model_invocable = false;
    assert!(ReadSkillReferenceTool::for_catalog(&hidden, source.clone()).is_none());
    hidden.definitions.clear();
    assert!(ReadSkillReferenceTool::for_catalog(&hidden, source.clone()).is_none());
    assert!(
        ReadSkillReferenceTool::for_catalog(&SkillCatalog::legacy_unavailable(), source.clone())
            .is_none()
    );
    assert!(source.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reference_read_uses_frozen_identity_and_returns_complete_content_without_activation() {
    let catalog = catalog(SkillSource::Builtin);
    let source = source(Response::Content(Some("exact reference body\n".to_owned())));
    let tool = ReadSkillReferenceTool::for_catalog(&catalog, source.clone()).unwrap();
    for reference in BUILTIN_GUIDE_REFERENCES {
        let input = tool
            .resolve(
                serde_json::from_value(
                    serde_json::json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":reference}),
                )
                .unwrap(),
            )
            .unwrap()
            .into_input();
        // 无 call_id／激活 latch 也可读，结果由普通 Tool Exchange 保存。
        let output =
            serde_json::to_value(tool.execute(input, ToolContext::default()).await.unwrap())
                .unwrap();
        assert_eq!(
            output,
            serde_json::json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":reference,"content":"exact reference body\n"})
        );
    }
    let requests = source.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for request in requests.iter() {
        assert_eq!(
            request.definition_digest,
            catalog.definitions[0].definition_digest
        );
        assert_eq!(request.name.as_str(), BUILTIN_GUIDE_SKILL_NAME);
    }
}

#[test]
fn unknown_or_path_like_references_fail_before_source_access() {
    let source = source(Response::Content(Some("never read".to_owned())));
    let tool = ReadSkillReferenceTool::for_catalog(&catalog(SkillSource::Builtin), source.clone())
        .unwrap();
    for reference in [
        "",
        "references/other.md",
        "/etc/passwd",
        "../references/mcp.md",
        "C:\\data",
        "https://example.invalid",
        "references\\mcp.md",
        "references/mcp.md\0",
        " references/mcp.md",
    ] {
        assert!(
            tool.resolve(
                serde_json::from_value(
                    serde_json::json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":reference})
                )
                .unwrap()
            )
            .is_err()
        );
    }
    assert!(
        tool.resolve(
            serde_json::from_value(
                serde_json::json!({"name":"other-skill","reference":BUILTIN_GUIDE_REFERENCES[0]})
            )
            .unwrap()
        )
        .is_err()
    );
    assert!(source.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unavailable_failed_and_oversized_content_are_errors_not_truncated_success() {
    for response in [
        Response::Content(None),
        Response::Failed,
        Response::Content(Some("x".repeat(MAX_SKILL_REFERENCE_BYTES + 1))),
    ] {
        let tool =
            ReadSkillReferenceTool::for_catalog(&catalog(SkillSource::Builtin), source(response))
                .unwrap();
        let input = tool.resolve(serde_json::from_value(serde_json::json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":BUILTIN_GUIDE_REFERENCES[0]})).unwrap()).unwrap().into_input();
        let error = tool
            .execute(input, ToolContext::default())
            .await
            .err()
            .expect("read error");
        assert!(!error.to_string().contains("private source failure"));
    }
}

#[tokio::test]
async fn cancellation_before_and_during_a_source_read_finishes_without_background_work() {
    let source = source(Response::Pending);
    let tool = ReadSkillReferenceTool::for_catalog(&catalog(SkillSource::Builtin), source.clone())
        .unwrap();
    let input = || {
        tool.resolve(serde_json::from_value(serde_json::json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":BUILTIN_GUIDE_REFERENCES[0]})).unwrap()).unwrap().into_input()
    };
    let context = ToolContext::default();
    context.cancellation.cancel();
    assert!(tool.execute(input(), context).await.is_err());
    assert!(source.requests.lock().unwrap().is_empty());
    let context = ToolContext::default();
    let cancellation = context.cancellation.clone();
    let mut read = tool.execute(input(), context);
    std::future::poll_fn(|cx| {
        assert!(read.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert_eq!(source.requests.lock().unwrap().len(), 1);
    cancellation.cancel();
    assert!(read.await.is_err());
}
