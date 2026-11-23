use super::*;
use rusqlite::TransactionBehavior;

fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE workspaces(workspace_id TEXT PRIMARY KEY, lifecycle TEXT, created_at_ms INTEGER, updated_at_ms INTEGER);
        CREATE TABLE sessions(session_id TEXT PRIMARY KEY, lifecycle TEXT, role TEXT, is_pinned INTEGER, created_at_ms INTEGER, updated_at_ms INTEGER);
        CREATE TABLE session_resources(session_id TEXT PRIMARY KEY, workspace_id TEXT);
        CREATE TABLE schedule_executions(session_id TEXT REFERENCES sessions(session_id));
        CREATE UNIQUE INDEX schedule_execution_owner ON schedule_executions(session_id) WHERE session_id IS NOT NULL;
        CREATE INDEX session_resources_workspace ON session_resources(workspace_id,session_id);
        INSERT INTO workspaces VALUES('w','active',1,10),('x','active',2,20),('removed','removed',3,30);
        INSERT INTO sessions VALUES('a','active','standard',1,1,40),('b','active','standard',0,2,20),('hidden','archived','standard',0,3,30),('c','active','standard',1,4,10),('controller','active','controller',0,5,50),('other','active','standard',0,6,60),('unbound','active','standard',0,7,70),('removed-session','active','standard',1,8,80);
        INSERT INTO session_resources VALUES('a','w'),('b','w'),('hidden','w'),('c','w'),('controller','w'),('other','x'),('unbound',NULL),('removed-session','removed');").unwrap();
    db.execute_batch(include_str!("../migrations/v0_33_1/sidebar_order.sql"))
        .unwrap();
    db
}

fn page(db: &Connection, scope: Scope<'_>) -> Vec<String> {
    scope.page(db, false, 0, None).unwrap()
}

fn move_item(
    db: &mut Connection,
    scope: Scope<'_>,
    item: &str,
    reference: &str,
    placement: Placement,
) {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    scope.move_item(&tx, item, reference, placement).unwrap();
    tx.commit().unwrap();
}

#[test]
fn migration_default_pages_filter_without_losing_hidden_positions() {
    let db = fixture();
    assert_eq!(page(&db, Scope::Workspaces), ["x", "w"]);
    assert_eq!(page(&db, Scope::Sessions(Some("w"))), ["c", "b", "a"]);
    assert_eq!(
        Scope::Sessions(Some("w"))
            .page(&db, true, 0, Some(20))
            .unwrap(),
        ["hidden"]
    );
    assert_eq!(
        Scope::Sessions(Some("w"))
            .page(&db, false, 1, Some(1))
            .unwrap(),
        ["b"]
    );
    assert_eq!(page(&db, Scope::Sessions(None)), ["unbound"]);
    assert_eq!(page(&db, Scope::Pins), ["a", "c"]);
}

#[test]
fn movement_uses_hidden_neighbor_and_reset_is_scope_local() {
    let mut db = fixture();
    let scope = Scope::Sessions(Some("w"));
    move_item(&mut db, scope, "a", "c", Placement::After);
    assert_eq!(
        scope.ids(&db).unwrap(),
        ["controller", "c", "a", "hidden", "b"]
    );
    assert_eq!(page(&db, scope), ["c", "a", "b"]);
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    scope.reset(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(page(&db, scope), ["c", "b", "a"]);
    assert_eq!(page(&db, Scope::Sessions(Some("x"))), ["other"]);
    assert_eq!(page(&db, Scope::Pins), ["a", "c"]);
    assert_eq!(
        db.query_row("SELECT sum(updated_at_ms) FROM sessions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        360
    );
}

#[test]
fn gap_exhaustion_reorders_and_hidden_rows_keep_relative_order() {
    let mut db = fixture();
    db.execute_batch("UPDATE session_resources SET sort_order=CASE session_id WHEN 'a' THEN 1 WHEN 'b' THEN 2 WHEN 'hidden' THEN 3 WHEN 'c' THEN 4 ELSE 5 END WHERE workspace_id='w'").unwrap();
    move_item(
        &mut db,
        Scope::Sessions(Some("w")),
        "a",
        "c",
        Placement::After,
    );
    assert_eq!(
        Scope::Sessions(Some("w")).ids(&db).unwrap(),
        ["controller", "c", "a", "hidden", "b"]
    );
    assert_eq!(
        db.query_row(
            "SELECT count(distinct sort_order) FROM session_resources WHERE workspace_id='w'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        5
    );
    // 后页移动至首页，查询先排序再 LIMIT。
    move_item(
        &mut db,
        Scope::Sessions(Some("w")),
        "b",
        "c",
        Placement::Before,
    );
    assert_eq!(
        Scope::Sessions(Some("w"))
            .page(&db, false, 0, Some(1))
            .unwrap(),
        ["b"]
    );
}

#[test]
fn overflow_allocation_and_move_stay_in_integer_range() {
    let mut db = fixture();
    db.execute(
        "UPDATE workspaces SET sort_order=?1 WHERE workspace_id='w'",
        [i64::MAX],
    )
    .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(
        Scope::Workspaces.next(&tx, Placement::Before).unwrap(),
        4096
    );
    tx.commit().unwrap();
    db.execute(
        "UPDATE sessions SET pin_order=?1 WHERE session_id='a'",
        [i64::MIN],
    )
    .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(Scope::Pins.next(&tx, Placement::After).unwrap(), 0);
    tx.commit().unwrap();
    db.execute(
        "UPDATE workspaces SET sort_order=?1 WHERE workspace_id='w'",
        [i64::MAX],
    )
    .unwrap();
    move_item(&mut db, Scope::Workspaces, "x", "w", Placement::Before);
    assert_eq!(page(&db, Scope::Workspaces), ["x", "w"]);
}

#[test]
fn invalid_moves_and_failed_reorder_leave_original_state() {
    let mut db = fixture();
    let scope = Scope::Sessions(Some("w"));
    let before = scope.ids(&db).unwrap();
    for (item, reference) in [
        ("a", "a"),
        ("a", "hidden"),
        ("a", "other"),
        ("a", "missing"),
        ("controller", "a"),
    ] {
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(
            scope
                .move_item(&tx, item, reference, Placement::Before)
                .is_err()
        );
    }
    assert_eq!(scope.ids(&db).unwrap(), before);
    db.execute_batch("UPDATE session_resources SET sort_order=1 WHERE workspace_id='w';CREATE TRIGGER reject_reorder BEFORE UPDATE ON session_resources WHEN NEW.session_id='b' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let before = scope.ids(&db).unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(scope.move_item(&tx, "a", "c", Placement::After).is_err());
    drop(tx);
    assert_eq!(scope.ids(&db).unwrap(), before);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM session_resources WHERE workspace_id='w' AND sort_order=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        5
    );
}

#[test]
fn reset_rejects_missing_or_removed_workspace_but_accepts_empty_active_group() {
    let mut db = fixture();
    let removed = Scope::Sessions(Some("removed"));
    let before: i64 = db
        .query_row(
            "SELECT sort_order FROM session_resources WHERE session_id='removed-session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for scope in [removed, Scope::Sessions(Some("missing"))] {
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(scope.reset(&tx).is_err());
    }
    assert_eq!(
        db.query_row(
            "SELECT sort_order FROM session_resources WHERE session_id='removed-session'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        before
    );
    db.execute(
        "INSERT INTO workspaces VALUES('empty','active',10,10,10000)",
        [],
    )
    .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    Scope::Sessions(Some("empty")).reset(&tx).unwrap();
    tx.commit().unwrap();
    assert!(page(&db, Scope::Sessions(Some("empty"))).is_empty());
}

#[test]
fn pins_are_independent_and_workspace_hidden_order_is_preserved() {
    let mut db = fixture();
    move_item(&mut db, Scope::Pins, "c", "a", Placement::Before);
    assert_eq!(page(&db, Scope::Pins), ["c", "a"]);
    assert_eq!(page(&db, Scope::Sessions(Some("w"))), ["c", "b", "a"]);
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(Scope::Pins.reset(&tx).is_err());
    drop(tx);
    move_item(&mut db, Scope::Workspaces, "w", "x", Placement::Before);
    db.execute(
        "UPDATE workspaces SET lifecycle='active' WHERE workspace_id='removed'",
        [],
    )
    .unwrap();
    assert_eq!(page(&db, Scope::Workspaces), ["removed", "w", "x"]);
}

#[test]
fn large_group_reorder_and_reset_use_indexes_and_keep_other_groups() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    std::fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f04-10k-")
        .tempdir_in(root)
        .unwrap();
    let mut db = Connection::open(home.path().join("runtime.sqlite3")).unwrap();
    rusqlite::backup::Backup::new(&fixture(), &mut db)
        .unwrap()
        .run_to_completion(256, std::time::Duration::ZERO, None)
        .unwrap();
    db.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10000)
        INSERT INTO sessions(session_id,lifecycle,role,is_pinned,created_at_ms,updated_at_ms) SELECT printf('large-%05d',i),'active','standard',0,i,i FROM n;
        INSERT INTO session_resources SELECT session_id,'large',created_at_ms FROM sessions WHERE session_id LIKE 'large-%';
        INSERT INTO workspaces VALUES('large','active',9,9,9999);").unwrap();
    let scope = Scope::Sessions(Some("large"));
    let started = std::time::Instant::now();
    move_item(
        &mut db,
        scope,
        "large-00001",
        "large-09000",
        Placement::After,
    );
    let move_elapsed = started.elapsed();
    let started = std::time::Instant::now();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    scope.reset(&tx).unwrap();
    tx.commit().unwrap();
    eprintln!(
        "F04 10k reorder {:?}; reset {:?}",
        move_elapsed,
        started.elapsed()
    );
    assert_eq!(
        scope.page(&db, false, 0, Some(2)).unwrap(),
        ["large-10000", "large-09999"]
    );
    assert_eq!(page(&db, Scope::Sessions(Some("w"))), ["c", "b", "a"]);
    for (query, index) in [
        (
            "SELECT workspace_id FROM workspaces WHERE lifecycle='active' ORDER BY sort_order DESC,workspace_id LIMIT 50",
            "workspaces_sort_order",
        ),
        (
            "SELECT r.session_id FROM session_resources r CROSS JOIN sessions s ON s.session_id=r.session_id WHERE workspace_id='large' AND s.lifecycle='active' AND role='standard' ORDER BY r.sort_order DESC,r.session_id LIMIT 20",
            "session_resources_workspace_order",
        ),
        (
            "SELECT session_id FROM sessions WHERE is_pinned=1 ORDER BY pin_order DESC,session_id LIMIT 2048",
            "sessions_pin_order",
        ),
        (
            "SELECT session_id FROM sessions ORDER BY created_at_ms DESC,session_id LIMIT 20",
            "sessions_created_order",
        ),
    ] {
        let plan: Vec<String> = db
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .unwrap()
            .query_map([], |r| r.get(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(plan.iter().any(|line| line.contains(index)), "{plan:?}");
        assert!(
            !plan.iter().any(|line| line.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}
