//! 在全新隔离文件库验证正式摘要查询；不存在正文文件的夹具必须也能分页。
use super::*;
use assistant_protocol::{
    GetApplicationSnapshotRequest, ListWorkspacesRequest, SessionGroup, SessionGroupRequest,
    SessionListFilter,
};
use assistant_runtime::SessionSummaryQuery;

#[test]
fn grouped_snapshot_pages_before_projection_and_keeps_pins_outside_project_prefix() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f04-pages-")
        .tempdir_in(root)
        .unwrap();
    let mut engine = open_engine(&home);
    // 本机 SQLite，无网络端口；仅此新建库的 workspaces/sessions/session_resources。
    for index in 0..52 {
        let name = format!("workspace-{index:02}");
        engine
            .register_workspace(NewWorkspaceRegistration {
                workspace_id: workspace_id(&name),
                label: name,
                requested_primary_directory: None,
                requested_additional_directories: vec![],
                changed_at_ms: index,
            })
            .unwrap();
    }
    let tx = engine.connection.transaction().unwrap();
    for group in 0..53 {
        for index in 0..25 {
            let id = format!("row-{group:02}-{index:02}");
            let workspace = (group < 52).then(|| format!("workspace-{group:02}"));
            tx.execute("INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms,is_pinned,pin_order) VALUES(?1,?1,'[]','[]','active',1,0,?2,?3,?4,?5)", rusqlite::params![id,index,99999-index,i64::from(index==0 && group==0),100-index]).unwrap();
            tx.execute("INSERT INTO session_resources(session_id,workspace_id,working_directory,attachment_directory,private_directory,created_at_ms,sort_order) VALUES(?1,?2,?1,?1,?1,?3,?4)", rusqlite::params![id,workspace,index,index*1024]).unwrap();
        }
    }
    tx.execute(
        "UPDATE sessions SET role='controller' WHERE session_id='row-52-24'",
        [],
    )
    .unwrap();
    tx.execute(
        "UPDATE sessions SET lifecycle='archived',archived_at_ms=100 WHERE session_id='row-52-23'",
        [],
    )
    .unwrap();
    tx.execute("UPDATE workspaces SET lifecycle='removed',removed_at_ms=100 WHERE workspace_id='workspace-01'", []).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1325
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM session_resources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1325
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        52
    );

    let started = Instant::now();
    let snapshot = engine
        .query_sidebar(GetApplicationSnapshotRequest::default())
        .unwrap();
    eprintln!(
        "F04 actual summary snapshot, 10 projects + unbound: {:?}",
        started.elapsed()
    );
    assert_eq!(snapshot.workspaces.workspaces.len(), 10);
    assert!(snapshot.workspaces.has_more);
    assert_eq!(
        snapshot.workspaces.workspaces[0].workspace_id.as_str(),
        "workspace-51"
    );
    assert!(
        !snapshot
            .workspaces
            .workspaces
            .iter()
            .any(|w| w.workspace_id.as_str() == "workspace-00")
    );
    assert_eq!(snapshot.session_groups.len(), 11);
    for group in &snapshot.session_groups {
        assert_eq!(group.sessions.len(), 10);
        assert_eq!(group.next_offset, Some(10));
    }
    assert_eq!(
        snapshot.session_groups[0].sessions[0].session_id.as_str(),
        "row-51-24"
    );
    assert_eq!(
        snapshot.session_groups[10].sessions[0].session_id.as_str(),
        "row-52-22"
    );
    assert_eq!(snapshot.pinned_sessions[0].session_id.as_str(), "row-00-00");
    let last = engine
        .query_workspaces(ListWorkspacesRequest {
            offset: 50,
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(last.workspaces.len(), 1);
    assert!(!last.has_more);
    assert_eq!(last.workspaces[0].workspace_id.as_str(), "workspace-00");
    assert_eq!(
        engine
            .query_workspaces(ListWorkspacesRequest::default())
            .unwrap()
            .workspaces
            .len(),
        51
    );

    let page = engine
        .query_session_summaries(SessionSummaryQuery {
            group: Some(SessionGroup::Unbound),
            offset: 20,
            limit: 21,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.len(), 3);
    assert_eq!(page[0].session_id.as_str(), "row-52-02");
    let custom = engine
        .query_sidebar(GetApplicationSnapshotRequest {
            workspaces_limit: Some(51),
            session_groups: Some(vec![
                SessionGroupRequest {
                    group: SessionGroup::Unbound,
                    filter: SessionListFilter::Archived,
                    limit: None,
                },
                SessionGroupRequest {
                    group: SessionGroup::Workspace {
                        workspace_id: workspace_id("workspace-00"),
                    },
                    filter: SessionListFilter::Active,
                    limit: Some(40),
                },
                SessionGroupRequest {
                    group: SessionGroup::Workspace {
                        workspace_id: workspace_id("workspace-01"),
                    },
                    filter: SessionListFilter::Active,
                    limit: None,
                },
            ]),
        })
        .unwrap();
    assert_eq!(custom.session_groups.len(), 3);
    assert_eq!(
        custom.session_groups[0].sessions[0].session_id.as_str(),
        "row-52-23"
    );
    assert_eq!(custom.session_groups[1].sessions.len(), 25);
    assert_eq!(custom.session_groups[1].next_offset, None);
    assert!(custom.session_groups[2].sessions.is_empty());
    assert!(
        engine
            .query_sidebar(GetApplicationSnapshotRequest {
                session_groups: Some(vec![]),
                ..Default::default()
            })
            .unwrap()
            .session_groups
            .is_empty()
    );
    let global = engine
        .query_session_summaries(SessionSummaryQuery {
            limit: 2000,
            ..Default::default()
        })
        .unwrap();
    assert!(
        global
            .windows(2)
            .all(|pair| pair[0].created_at_ms > pair[1].created_at_ms
                || (pair[0].created_at_ms == pair[1].created_at_ms
                    && pair[0].session_id < pair[1].session_id))
    );
    assert_ne!(global[0].session_id.as_str(), "row-00-00");
    assert!(engine.connection.is_autocommit());
}
