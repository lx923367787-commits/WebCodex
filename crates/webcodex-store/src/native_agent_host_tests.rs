use super::*;
const TUNNEL: &str = "tunnel_0123456789abcdef0123456789abcdef";
const DIGEST: &str = "ee9d4a75bc0b42f36f345aa96231e0db1ab00488122f34ebc99d6db055b6603e";

#[test]
fn native_host_rotation_reopen_and_revocation_keep_stable_principal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let db = Database::open(&path).unwrap();
    let first = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    let first_token = first.into_secret();
    let verified = db.authenticate_native_agent_host(&first_token).unwrap();
    assert_eq!(
        db.native_agent_host_principal(verified.host_id()).unwrap(),
        verified.principal_key()
    );
    let hashes: String = db
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT token_hash FROM wc_native_agent_hosts", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(hashes, first_token);
    drop(db);
    let db = Database::open(&path).unwrap();
    db.validate_native_agent_host(&verified).unwrap();
    let second = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    assert_eq!(second.host_id, verified.host_id());
    assert_eq!(second.generation, verified.generation() + 1);
    let second_token = second.into_secret();
    let current = db.authenticate_native_agent_host(&second_token).unwrap();
    assert_eq!(current.principal_key(), verified.principal_key());
    assert!(db.authenticate_native_agent_host(&first_token).is_err());
    assert!(db.validate_native_agent_host(&verified).is_err());
    db.revoke_native_agent_host(current.host_id()).unwrap();
    assert!(db.authenticate_native_agent_host(&second_token).is_err());
    assert!(db.register_native_agent_host(TUNNEL, DIGEST).is_err());
}

#[test]
fn native_host_does_not_authenticate_agent_and_never_accepts_asserted_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let grant = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    let host_id = grant.host_id.clone();
    let secret = grant.into_secret();
    assert!(db.authenticate_agent_run(&secret).is_err());
    for value in ["wc_nh_fake", "wc_pat_fake", "wc_ar_fake", "bootstrap"] {
        assert!(db.authenticate_native_agent_host(value).is_err());
    }
    assert_eq!(
        db.native_agent_host_principal(&host_id).unwrap(),
        db.authenticate_native_agent_host(&secret)
            .unwrap()
            .principal_key()
    );
    assert!(db.native_agent_host_principal("asserted-host").is_err());
}

#[test]
fn native_host_concurrent_rotations_leave_one_credential_current() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let db = Database::open(&path).unwrap();
    let host_id = db
        .register_native_agent_host(TUNNEL, DIGEST)
        .unwrap()
        .host_id;
    let other = Database::open(&path).unwrap();
    let join =
        std::thread::spawn(move || other.register_native_agent_host(TUNNEL, DIGEST).unwrap());
    let a = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    let b = join.join().unwrap();
    assert_eq!(a.host_id, host_id);
    assert_eq!(b.host_id, host_id);
    let live = [a.into_secret(), b.into_secret()]
        .iter()
        .filter(|s| db.authenticate_native_agent_host(s).is_ok())
        .count();
    assert_eq!(live, 1);
}

fn auto_enrollment_fixture() -> (tempfile::TempDir, Database, VerifiedNativeAgentHost) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    db.create_user(&crate::models::UserRecord {
        id: "native-owner".into(),
        username: "native-owner".into(),
        created_at: 1,
        disabled: 0,
        display_name: None,
        role: "user".into(),
        disabled_at: None,
        updated_at: None,
    })
    .unwrap();
    let token = db
        .register_native_agent_host(TUNNEL, DIGEST)
        .unwrap()
        .into_secret();
    let host = db.authenticate_native_agent_host(&token).unwrap();
    (dir, db, host)
}

fn configure_auto(db: &Database, host: &VerifiedNativeAgentHost) {
    db.configure_native_host_auto_enrollment(
        host.host_id(),
        "native-owner",
        "project",
        crate::AgentEnrollmentPolicy {
            max_subagents: 2,
            max_cli_calls: 1,
        },
    )
    .unwrap();
}

#[test]
fn native_auto_enrollment_is_explicit_atomic_independent_and_idempotent() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let first = "a".repeat(64);
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(
        db.ensure_native_host_window(&host, &first, now)
            .unwrap_err()
            .code(),
        "agent_host_enrollment_required"
    );
    assert_eq!(
        db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    configure_auto(&db, &host);
    let a = db.ensure_native_host_window(&host, &first, now).unwrap();
    let again = db.ensure_native_host_window(&host, &first, now).unwrap();
    let b = db
        .ensure_native_host_window(&host, &"b".repeat(64), now)
        .unwrap();
    assert_eq!(a, again);
    assert_ne!(a.agent_id(), b.agent_id());
    assert_ne!(a.run_id(), b.run_id());
    assert_eq!(a.owner_user(), "native-owner");
    assert_eq!(a.project(), "project");
    let conn = db.conn.lock().unwrap();
    for table in [
        "wc_agent_tasks",
        "wc_agent_enrollments",
        "wc_agent_host_windows",
    ] {
        assert_eq!(
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(DISTINCT task_directory) FROM wc_agent_enrollments",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}

#[test]
fn native_auto_enrollment_never_reuses_stale_or_foreign_window() {
    for stale in ["revoked", "pending", "disabled", "foreign"] {
        let (_dir, db, host) = auto_enrollment_fixture();
        configure_auto(&db, &host);
        let window = "c".repeat(64);
        let now = chrono::Utc::now().timestamp_millis();
        let run = db.ensure_native_host_window(&host, &window, now).unwrap();
        {
            let conn = db.conn.lock().unwrap();
            match stale {
                "disabled" => {
                    conn.execute("UPDATE wc_agent_host_windows SET enabled=0", [])
                        .unwrap();
                }
                "foreign" => {
                    conn.execute(
                        "UPDATE wc_agent_host_windows SET principal_key='another-host'",
                        [],
                    )
                    .unwrap();
                }
                _ => {
                    conn.execute(
                        "UPDATE wc_agent_runs SET state=?1 WHERE run_id=?2",
                        params![stale, run.run_id()],
                    )
                    .unwrap();
                }
            }
        }
        assert_eq!(
            db.ensure_native_host_window(&host, &window, now)
                .unwrap_err()
                .code(),
            "agent_host_window_not_active",
            "{stale}"
        );
        assert_eq!(
            db.conn
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn native_auto_enrollment_revalidates_user_and_host_and_rolls_back_failures() {
    let (_dir, db, host) = auto_enrollment_fixture();
    configure_auto(&db, &host);
    let now = chrono::Utc::now().timestamp_millis();
    let run = db
        .ensure_native_host_window(&host, &"a".repeat(64), now)
        .unwrap();
    db.conn
        .lock()
        .unwrap()
        .execute("UPDATE users SET disabled_at=1", [])
        .unwrap();
    for window in ["a".repeat(64), "b".repeat(64)] {
        assert_eq!(
            db.ensure_native_host_window(&host, &window, now)
                .unwrap_err()
                .code(),
            "agent_enrollment_owner_unavailable"
        );
    }
    db.conn
        .lock()
        .unwrap()
        .execute("UPDATE users SET disabled_at=NULL", [])
        .unwrap();
    // A failure after the public task insert must roll the complete transaction back.
    db.conn.lock().unwrap().execute_batch("CREATE TRIGGER reject_native_claim BEFORE INSERT ON wc_agent_host_windows BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    assert!(db
        .ensure_native_host_window(&host, &"b".repeat(64), now)
        .is_err());
    assert_eq!(
        db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.ensure_native_host_window(&host, &"a".repeat(64), now)
            .unwrap(),
        run
    );
    db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    assert_eq!(
        db.ensure_native_host_window(&host, &"a".repeat(64), now)
            .unwrap_err()
            .code(),
        "native_host_invalid"
    );
}

#[test]
fn native_auto_enrollment_concurrent_first_calls_create_one_agent() {
    let (dir, db, host) = auto_enrollment_fixture();
    configure_auto(&db, &host);
    let other = Database::open(&dir.path().join("state.db")).unwrap();
    let second_host = host.clone();
    let now = chrono::Utc::now().timestamp_millis();
    let join = std::thread::spawn(move || {
        other
            .ensure_native_host_window(&second_host, &"d".repeat(64), now)
            .unwrap()
    });
    let a = db
        .ensure_native_host_window(&host, &"d".repeat(64), now)
        .unwrap();
    assert_eq!(a, join.join().unwrap());
    assert_eq!(
        db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
