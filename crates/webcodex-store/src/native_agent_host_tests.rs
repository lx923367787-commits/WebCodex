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
