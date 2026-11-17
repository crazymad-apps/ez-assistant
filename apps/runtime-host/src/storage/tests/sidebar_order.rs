use super::*;
use crate::storage::sidebar_order::{Placement, Scope};
use rusqlite::TransactionBehavior;

fn home() -> TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("f04-store-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn sidebar_order_creation_pin_retry_and_reopen_preserve_positions() {
    let home = home();
    let mut engine = open_engine(&home);
    for name in ["a", "b", "c"] {
        engine
            .create_session(new_session(name, &engine.sessions_directory))
            .unwrap();
    }
    let page = |engine: &StorageEngine, scope: Scope<'_>| {
        scope.page(&engine.connection, false, 0, None).unwrap()
    };
    assert_eq!(page(&engine, Scope::Sessions(None)), ["c", "b", "a"]);
    for name in ["a", "c", "a"] {
        engine
            .set_session_pinned(SessionPinnedChange {
                changed_at_ms: 12345,
                session_id: session_id(name),
                is_pinned: true,
            })
            .unwrap();
    }
    assert_eq!(page(&engine, Scope::Pins), ["a", "c"]);
    engine
        .set_session_pinned(SessionPinnedChange {
            changed_at_ms: 12345,
            session_id: session_id("a"),
            is_pinned: false,
        })
        .unwrap();
    engine
        .set_session_pinned(SessionPinnedChange {
            changed_at_ms: 12345,
            session_id: session_id("a"),
            is_pinned: true,
        })
        .unwrap();
    assert_eq!(page(&engine, Scope::Pins), ["c", "a"]);
    let tx = engine
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    Scope::Sessions(None)
        .move_item(&tx, "a", "c", Placement::Before)
        .unwrap();
    tx.commit().unwrap();
    engine.connection.execute("UPDATE sessions SET title='renamed',updated_at_ms=99999,lifecycle='archived' WHERE session_id='b'",[]).unwrap();
    drop(engine);
    let engine = open_engine(&home);
    assert_eq!(page(&engine, Scope::Sessions(None)), ["a", "c"]);
    engine
        .connection
        .execute(
            "UPDATE sessions SET lifecycle='active' WHERE session_id='b'",
            [],
        )
        .unwrap();
    assert_eq!(page(&engine, Scope::Sessions(None)), ["a", "c", "b"]);
    assert_eq!(page(&engine, Scope::Pins), ["c", "a"]);
}

#[test]
fn sidebar_order_workspaces_and_resource_repair_assign_orders() {
    let home = home();
    let mut engine = open_engine(&home);
    for name in ["w-a", "w-b"] {
        engine
            .register_workspace(NewWorkspaceRegistration {
                workspace_id: workspace_id(name),
                label: name.into(),
                requested_primary_directory: None,
                requested_additional_directories: vec![],
                changed_at_ms: 100,
            })
            .unwrap();
    }
    assert_eq!(
        Scope::Workspaces
            .page(&engine.connection, false, 0, None)
            .unwrap(),
        ["w-b", "w-a"]
    );
    for name in ["a", "b"] {
        engine
            .create_session(new_session(name, &engine.sessions_directory))
            .unwrap();
    }
    engine
        .connection
        .execute("DELETE FROM session_resources WHERE session_id='a'", [])
        .unwrap();
    engine
        .repair_session_resources_scoped(Some(&session_id("a")))
        .unwrap();
    assert_eq!(
        Scope::Sessions(None)
            .page(&engine.connection, false, 0, None)
            .unwrap(),
        ["a", "b"]
    );
    let before: i64 = engine
        .connection
        .query_row(
            "SELECT sort_order FROM session_resources WHERE session_id='a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    engine
        .repair_session_resources_scoped(Some(&session_id("a")))
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT sort_order FROM session_resources WHERE session_id='a'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        before
    );
    drop(engine);
    open_engine(&home);
}

#[test]
fn sidebar_commands_validate_current_scope_and_reset_hidden_items_without_metadata_changes() {
    use assistant_protocol::{
        MoveSidebarItemRequest, ResetSidebarOrderRequest, SessionGroup, SidebarPlacement,
    };
    // 本机 SQLite，无端口；新建隔离库，仅测试 workspaces/sessions/session_resources。
    let home = home();
    let mut engine = open_engine(&home);
    for (index, name) in ["a", "b", "c"].iter().enumerate() {
        let mut session = new_session(name, &engine.sessions_directory);
        session.created_at_ms = index as i64;
        engine.create_session(session).unwrap();
    }
    let request = |item, reference| MoveSidebarItemRequest::Sessions {
        group: SessionGroup::Unbound,
        session_id: session_id(item),
        reference_id: session_id(reference),
        placement: SidebarPlacement::Before,
    };
    engine.move_sidebar_item(request("a", "c")).unwrap();
    assert_eq!(
        Scope::Sessions(None)
            .page(&engine.connection, false, 0, None)
            .unwrap(),
        ["a", "c", "b"]
    );
    let metadata = |engine: &StorageEngine| -> Vec<(String, String, i64, i64)> {
        engine.connection.prepare("SELECT session_id,title,created_at_ms,updated_at_ms FROM sessions ORDER BY session_id").unwrap()
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap().collect::<Result<_,_>>().unwrap()
    };
    let before = metadata(&engine);
    engine
        .connection
        .execute(
            "UPDATE sessions SET lifecycle='archived' WHERE session_id='c'",
            [],
        )
        .unwrap();
    assert_eq!(
        engine
            .move_sidebar_item(request("b", "c"))
            .unwrap_err()
            .kind(),
        assistant_runtime::StoreErrorKind::Conflict
    );
    assert_eq!(
        Scope::Sessions(None)
            .page(&engine.connection, false, 0, None)
            .unwrap(),
        ["a", "b"]
    );
    assert!(
        engine
            .move_sidebar_item(MoveSidebarItemRequest::Sessions {
                group: SessionGroup::Workspace {
                    workspace_id: workspace_id("missing")
                },
                session_id: session_id("a"),
                reference_id: session_id("b"),
                placement: SidebarPlacement::Before
            })
            .is_err()
    );
    engine
        .reset_sidebar_order(ResetSidebarOrderRequest::Sessions {
            group: SessionGroup::Unbound,
        })
        .unwrap();
    engine
        .connection
        .execute(
            "UPDATE sessions SET lifecycle='active' WHERE session_id='c'",
            [],
        )
        .unwrap();
    assert_eq!(
        Scope::Sessions(None)
            .page(&engine.connection, false, 0, None)
            .unwrap(),
        ["c", "b", "a"]
    );
    assert_eq!(metadata(&engine), before);
    for name in ["a", "b"] {
        engine
            .set_session_pinned(SessionPinnedChange {
                session_id: session_id(name),
                is_pinned: true,
                changed_at_ms: 999,
            })
            .unwrap();
    }
    engine
        .move_sidebar_item(MoveSidebarItemRequest::Pins {
            session_id: session_id("b"),
            reference_id: session_id("a"),
            placement: SidebarPlacement::Before,
        })
        .unwrap();
    assert_eq!(
        Scope::Pins
            .page(&engine.connection, false, 0, None)
            .unwrap(),
        ["b", "a"]
    );
    assert_eq!(
        Scope::Sessions(None)
            .page(&engine.connection, false, 0, None)
            .unwrap(),
        ["c", "b", "a"]
    );
    for table in ["sessions", "session_resources"] {
        assert_eq!(
            engine
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
    }
    assert_eq!(
        engine
            .connection
            .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
