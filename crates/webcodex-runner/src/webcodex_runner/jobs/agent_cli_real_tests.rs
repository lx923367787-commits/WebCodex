//! Real synthetic helper + Runner proof path. No installed provider/account is
//! queried. Reconstructing a manager/new sink simulates instance replacement;
//! it is not represented as a real service/process restart or a Store commit.
use super::*;
use webcodex_core::agent_cli::{
    sha256, AgentCliCatalogQuery, AgentCliDeliveryManifest, AgentCliDeliveryManifestFile,
    AgentCliDeliveryQuery,
};
use webcodex_core::runner_protocol::AgentExecutionTarget;

fn delivery_query(start: &AgentExecutionStart, instance: &str) -> AgentCliDeliveryQuery {
    AgentCliDeliveryQuery {
        version: 1,
        target: AgentExecutionTarget {
            client_id: "native-runner".into(),
            runner_instance_id: instance.into(),
        },
        binding: start.binding.clone(),
        deadline_at_ms: chrono::Utc::now().timestamp_millis() + 40_000,
    }
}

#[test]
#[ignore = "real fixed metadata commands against synthetic helper only"]
fn agent_cli_real_catalog_uses_pinned_provider_and_does_not_attest_accounts() {
    for provider in [AgentCliProvider::WorkBuddy, AgentCliProvider::Antigravity] {
        let fixture = Fixture::new_with_provider(Some(provider));
        let query = AgentCliCatalogQuery {
            version: 1,
            target: AgentExecutionTarget {
                client_id: "native-runner".into(),
                runner_instance_id: "native-instance".into(),
            },
            provider,
            deadline_at_ms: chrono::Utc::now().timestamp_millis() + 40_000,
        };
        let catalog = fixture
            .manager
            .agent_cli_catalog(&fixture.sink, &query)
            .unwrap();
        let resource = fixture
            .profiles
            .descriptors()
            .into_iter()
            .find(|resource| resource.provider == Some(provider))
            .unwrap();
        catalog.validate_for(&query, &resource).unwrap();
        assert_eq!(catalog.cli_version, "1.2.3");
        assert_eq!(catalog.models, ["synthetic-model-a", "synthetic-model-b"]);
        assert_eq!(catalog.efforts, ["low", "high"]);
        assert!(!catalog.account_availability_verified);
        assert!(!catalog.nested_delegation_verified_disabled);
        assert_eq!(catalog.expires_at_ms - catalog.observed_at_ms, 60_000);
        assert!(
            fixture
                .configuration
                .store
                .read_agent_execution_metadata()
                .unwrap()
                .jobs
                .is_empty(),
            "metadata is not a business Job launch"
        );
        let mut expired = query.clone();
        expired.deadline_at_ms = chrono::Utc::now().timestamp_millis() + 31_000;
        assert_eq!(
            fixture
                .manager
                .agent_cli_catalog(&fixture.sink, &expired)
                .unwrap_err(),
            "agent_cli_control_deadline_expired"
        );
        let mut wrong = query;
        wrong.target.runner_instance_id = "stale-instance".into();
        assert_eq!(
            fixture
                .manager
                .agent_cli_catalog(&fixture.sink, &wrong)
                .unwrap_err(),
            "agent_cli_target_mismatch"
        );
    }
}

#[test]
#[ignore = "real LPAC explicit publish snapshot gated by actual full close proof"]
fn agent_cli_real_explicit_snapshot_requires_tree_proof_and_replays_on_new_reader_instance() {
    let fixture = Fixture::new();
    let artifact = "Synthetic explicit artifact\n";
    let manifest = AgentCliDeliveryManifest {
        version: 1,
        summary: "Explicit synthetic delivery".into(),
        files: vec![AgentCliDeliveryManifestFile {
            path: "files/artifact.txt".into(),
            sha256: sha256(artifact.as_bytes()),
            bytes: artifact.len() as u64,
        }],
    };
    manifest.validate().unwrap();
    let start = fixture.start(
        0,
        vec![
            "lpac-publish-delivery".into(),
            serde_json::to_string(&manifest).unwrap(),
            artifact.into(),
            "delivery-ready".into(),
            "release".into(),
        ],
        None,
    );
    fixture.enqueue(&start);
    let workspace = fixture.workspace(&start);
    fixture.wait_ready(&start, &[workspace.join("delivery-ready")], 1);
    let query = delivery_query(&start, "native-instance");
    assert_eq!(
        fixture
            .manager
            .agent_cli_delivery_snapshot(&fixture.sink, &query)
            .unwrap_err(),
        "agent_cli_delivery_stop_unproven"
    );
    let close = fixture.close(&[start.clone()]);
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &close)
        .unwrap();
    fixture.assert_tree_receipt(&start, &receipt.receipts[0]);
    let snapshot = fixture
        .manager
        .agent_cli_delivery_snapshot(&fixture.sink, &query)
        .unwrap();
    let canonical = snapshot.validate_for(&start.binding).unwrap();
    assert_eq!(snapshot.artifact_sha256, sha256(&canonical));
    assert_eq!(snapshot.bundle.summary, "Explicit synthetic delivery");
    assert_eq!(snapshot.bundle.files.len(), 1);
    assert_eq!(snapshot.bundle.files[0].path, "files/artifact.txt");
    assert_eq!(snapshot.bundle.files[0].text, artifact);
    let encoded = String::from_utf8(canonical).unwrap();
    assert!(!encoded.contains("must-not-be-delivered"));
    assert_eq!(
        fixture
            .manager
            .agent_cli_delivery_snapshot(&fixture.sink, &query)
            .unwrap(),
        snapshot
    );

    let (tx, _receiver) = tokio::sync::mpsc::channel(8);
    let reader_sink = RunnerSink::WebSocket {
        tx,
        client_id: "native-runner".into(),
        runner_instance_id: "new-reader-instance".into(),
    };
    // An upgraded/removed executable must not strand an already verified
    // delivery. Alter only this fixture's unused original helper copy.
    fs::write(&fixture.program, b"upgraded synthetic provider copy").unwrap();
    let reopened = JobManager::new(1);
    assert!(reopened
        .configure_agent_execution_resources(
            fixture.profiles.clone(),
            fixture.metadata_root.clone()
        )
        .is_err());
    reopened
        .configure_agent_delivery_history(fixture.metadata_root.clone())
        .unwrap();
    assert!(!reopened.agent_execution_ready());
    let reader_query = delivery_query(&start, "new-reader-instance");
    assert_ne!(reader_query.target, reader_query.binding.target);
    assert_eq!(
        reopened
            .agent_cli_delivery_snapshot(&reader_sink, &reader_query)
            .unwrap(),
        snapshot
    );
    assert!(
        lock_unpoison(&reopened.agent_execution).live.is_empty(),
        "read-only proof replay must not recreate a worker"
    );
    let mut wrong_target = reader_query.clone();
    wrong_target.target.client_id = "different-client".into();
    assert!(reopened
        .agent_cli_delivery_snapshot(&reader_sink, &wrong_target)
        .is_err());
    let mut unknown = reader_query;
    unknown.binding.executor.run.agent_run_id = "unknown-run".into();
    assert_eq!(
        reopened
            .agent_cli_delivery_snapshot(&reader_sink, &unknown)
            .unwrap_err(),
        "agent_cli_delivery_job_not_recorded"
    );
    let mut blocked = fixture.start(1, vec!["sleep".into(), "30".into()], None);
    blocked.binding.target = unknown.target.clone();
    reopened.enqueue(reader_sink.clone(), fixture.pending(&blocked));
    assert_eq!(
        lock_unpoison(&reopened.jobs)[job_id(&blocked.binding).unwrap()]
            .snapshot
            .error
            .as_deref(),
        Some("agent_execution_unavailable")
    );
    assert_eq!(
        fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap()
            .jobs
            .len(),
        1
    );
    assert!(lock_unpoison(&reopened.agent_execution).live.is_empty());
}

#[test]
#[ignore = "read-only history recovery on fresh validation metadata; no native launch"]
fn agent_cli_real_history_never_initializes_missing_metadata_or_lock() {
    for missing in [".agent-execution.json", ".root.lock"] {
        let fixture = Fixture::new();
        let removed = fixture.metadata_root.join(missing);
        fs::remove_file(&removed)
            .expect("remove only named synthetic fixture state for recovery test");
        let readonly = JobManager::new(1);
        assert!(readonly
            .configure_agent_delivery_history(fixture.metadata_root.clone())
            .is_err());
        assert!(!removed.exists(), "history read recreated missing state");
        assert!(!readonly.agent_execution_ready());
        let native = JobManager::new(1);
        assert!(native
            .configure_agent_execution_resources(
                fixture.profiles.clone(),
                fixture.metadata_root.clone()
            )
            .is_err());
        assert!(
            !removed.exists(),
            "existing resource root was used to reset missing metadata"
        );
    }
}
