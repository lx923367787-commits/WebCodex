use super::tests::{access, poll, registration, registry, reserved_start, resources};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use webcodex_core::agent_cli::AgentCliProvider;
use webcodex_core::agent_cli_controls::{ReviewedAgentCliControls, ReviewedControlArg};
use webcodex_core::runner_protocol::compute_launch_intent_hash;

struct Admission(AtomicUsize);
impl AgentExecutionStoreAdmission for Admission {
    fn validate_persisted(&self, _: &AgentExecutionBinding) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn synthetic_record() -> ReviewedAgentCliControls {
    ReviewedAgentCliControls {
        version: 1,
        record_id: "synthetic-unverified-review".into(),
        provider: AgentCliProvider::WorkBuddy,
        cli_version: "synthetic-1".into(),
        executable_sha256: "e".repeat(64),
        resource_set_id: "resources-0".into(),
        payload_sha256: "b".repeat(64),
        control_files: vec![],
        loading_method: "synthetic-unverified-loader".into(),
        fixed_args: vec![ReviewedControlArg {
            flag: "--tools".into(),
            value: Some(String::new()),
        }],
        verification_evidence_sha256: "c".repeat(64),
        independent_review_sha256: "d".repeat(64),
        adapter: None,
    }
}
fn controlled_resource() -> AgentExecutionResourceDescriptor {
    let controls = synthetic_record();
    AgentExecutionResourceDescriptor {
        project_workspace_copy: false,
        resource_set_id: controls.resource_set_id.clone(),
        executable_identity: format!("sha256:{}", controls.executable_sha256),
        manifest_hash: controls.execution_manifest_sha256().unwrap(),
        provider: Some(controls.provider),
        reviewed_controls: Some(controls),
    }
}
fn set_controlled_process(
    start: &mut AgentExecutionStart,
    resource: &AgentExecutionResourceDescriptor,
) {
    start.process.args = [
        "--model",
        "vendor/model",
        "--effort",
        "high",
        "--print",
        "--input-format",
        "text",
        "--output-format",
        "json",
        "--tools",
        "",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    start.binding.resource_manifest_hash = resource.manifest_hash.clone();
    start.binding.launch_intent_hash =
        compute_launch_intent_hash(&start.process, start.stdin.as_deref(), start.timeout_secs)
            .unwrap();
}

#[tokio::test]
async fn reviewed_controls_missing_record_is_readable_metadata_but_cannot_prepare() {
    let registry = registry(true).await;
    let mut facts = resources();
    facts[0].provider = Some(AgentCliProvider::WorkBuddy);
    registry
        .register_with_auth(registration(true, facts.clone()), Some(&access()))
        .await
        .unwrap();
    let (reservation, start) = reserved_start(&registry, 0).await;
    assert_eq!(
        registry
            .read_agent_execution_resource(&access(), reservation.target(), "resources-0")
            .await
            .unwrap(),
        facts[0]
    );
    let error = registry
        .prepare_agent_execution(&access(), &reservation, start, "controller".into())
        .await
        .err()
        .unwrap();
    assert_eq!(error, "agent_cli_reviewed_controls_required");
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn reviewed_controls_consistent_hashes_are_facts_not_supported_execution() {
    let registry = registry(true).await;
    let mut facts = resources();
    facts[0] = controlled_resource();
    registry
        .register_with_auth(registration(true, facts.clone()), Some(&access()))
        .await
        .unwrap();
    let (reservation, mut start) = reserved_start(&registry, 0).await;
    set_controlled_process(&mut start, &facts[0]);
    let actual = registry
        .read_agent_execution_resource(&access(), reservation.target(), "resources-0")
        .await
        .unwrap();
    assert_eq!(actual, facts[0]);
    actual.validate().unwrap();
    let error = registry
        .prepare_agent_execution(&access(), &reservation, start, "controller".into())
        .await
        .err()
        .unwrap();
    assert_eq!(error, "agent_cli_controls_mechanism_unverified");
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn reviewed_controls_registration_rejects_downgrade_and_subject_tampering_atomically() {
    let registry = registry(true).await;
    let (reservation, _) = reserved_start(&registry, 0).await;
    let original = registry
        .read_agent_execution_resource(&access(), reservation.target(), "resources-0")
        .await
        .unwrap();
    for mutation in [
        "provider_none",
        "record_payload",
        "manifest",
        "executable",
        "resource",
        "override",
    ] {
        let mut facts = resources();
        facts[0] = controlled_resource();
        match mutation {
            "provider_none" => facts[0].provider = None,
            "record_payload" => {
                facts[0].reviewed_controls.as_mut().unwrap().payload_sha256 = "f".repeat(64)
            }
            "manifest" => facts[0].manifest_hash = "f".repeat(64),
            "executable" => facts[0].executable_identity = format!("sha256:{}", "f".repeat(64)),
            "resource" => facts[0].resource_set_id = "changed-resource".into(),
            _ => {
                facts[0].reviewed_controls.as_mut().unwrap().fixed_args[0].flag =
                    "--allowedTools".into()
            }
        }
        assert!(
            registry
                .register_with_auth(registration(true, facts), Some(&access()))
                .await
                .is_err(),
            "{mutation}"
        );
        assert_eq!(
            registry
                .read_agent_execution_resource(&access(), reservation.target(), "resources-0")
                .await
                .unwrap(),
            original
        );
    }
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn reviewed_controls_changed_provider_is_fenced_before_admission_and_at_dequeue() {
    for queued in [false, true] {
        let registry = registry(true).await;
        let controlled = controlled_resource();
        // A non-provider synthetic fixture preserves the D4 generic path. Its
        // identical F does not let later provider metadata bypass the new gate.
        let mut original_facts = resources();
        original_facts[0].manifest_hash = controlled.manifest_hash.clone();
        registry
            .register_with_auth(registration(true, original_facts), Some(&access()))
            .await
            .unwrap();
        let (reservation, mut start) = reserved_start(&registry, 0).await;
        set_controlled_process(&mut start, &controlled);
        let binding = start.binding.clone();
        let prepared = registry
            .prepare_agent_execution(&access(), &reservation, start, "controller".into())
            .await
            .unwrap();
        let admission = Admission(AtomicUsize::new(0));
        if queued {
            assert!(
                registry
                    .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
                    .await
                    .unwrap()
                    .dispatched
            );
        }
        let mut changed = resources();
        changed[0] = controlled;
        registry
            .register_with_auth(registration(true, changed), Some(&access()))
            .await
            .unwrap();
        if !queued {
            assert_eq!(
                registry
                    .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
                    .await
                    .err()
                    .unwrap(),
                "agent_cli_controls_mechanism_unverified"
            );
            assert_eq!(admission.0.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(admission.0.load(Ordering::SeqCst), 1);
        }
        assert!(poll(&registry).await.is_none());
        assert_eq!(prepared.binding(), &binding);
    }
}
