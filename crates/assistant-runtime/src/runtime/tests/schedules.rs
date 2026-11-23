use super::*;
use assistant_protocol::{
    ContextPageRequest, ListSchedulesRequest, ScheduleTarget, UpdateScheduleRequest,
};

#[tokio::test]
async fn volatile_store_lists_replaces_and_deletes_schedules() {
    let runtime = runtime(Arc::new(NeverModel {
        capabilities: ModelCapabilities::default(),
    }));
    let created = runtime
        .create_schedule(
            serde_json::from_value(json!({
                "name":"fixture", "enabled":true,
                "rule":{"type":"weekly","weekdays":["monday","friday"],"time":"09:00"},
                "action":{"type":"script","command":"echo fixture"}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(created.next_run_at.is_some());
    let target = ScheduleTarget {
        schedule_id: created.schedule_id.clone(),
    };
    let mut settings = created.settings;
    assert!(settings.report_to_controller);
    settings.configuration.enabled = false;
    let paused = runtime
        .update_schedule(UpdateScheduleRequest {
            expected_settings: None,
            schedule_id: target.schedule_id.clone(),
            settings,
        })
        .await
        .unwrap();
    assert!(paused.next_run_at.is_none());
    let page = runtime
        .list_schedules(ListSchedulesRequest {
            page: Some(ContextPageRequest {
                offset: Some(0),
                limit: Some(1),
            }),
        })
        .await
        .unwrap();
    assert_eq!(page.page.total, 1);
    assert_eq!(page.schedules, [paused]);
    runtime.delete_schedule(target.clone()).await.unwrap();
    assert!(matches!(
        runtime.get_schedule(target).await,
        Err(RuntimeError::ScheduleNotFound)
    ));
}

#[tokio::test]
async fn scheduler_keeps_the_user_runtime_alive_only_while_a_script_has_a_deadline() {
    struct Executor;
    impl crate::ScheduleScriptExecutor for Executor {
        fn execute<'a>(
            &'a self,
            _: &'a assistant_protocol::ScheduleId,
            _: &'a assistant_protocol::ScheduleExecutionId,
            _: Option<&'a crate::StoredWorkspace>,
            _: String,
            _: tokio_util::sync::CancellationToken,
        ) -> crate::ScheduleScriptFuture<'a> {
            panic!("future fixture must never start its script")
        }
    }

    let runtime = Arc::new(runtime(Arc::new(NeverModel {
        capabilities: ModelCapabilities::default(),
    })));
    runtime.start_schedules(Arc::new(Executor)).await.unwrap();
    assert!(!runtime.has_background_work());
    let mut work = runtime.subscribe_work_changes();
    let plan = runtime
        .create_schedule(
            serde_json::from_value(json!({
                "name":"fixture", "enabled":true,
                "rule":{"type":"once","date":"2099-01-01","time":"09:00"},
                "action":{"type":"script","command":"echo fixture"}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(runtime.has_background_work());
    let mut settings = plan.settings;
    settings.configuration.enabled = false;
    runtime
        .update_schedule(UpdateScheduleRequest {
            expected_settings: None,
            schedule_id: plan.schedule_id,
            settings,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while runtime.has_background_work() {
            work.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}
