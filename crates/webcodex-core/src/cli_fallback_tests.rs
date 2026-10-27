use super::*;
use serde_json::json;

fn failed_job() -> ShellJobInfo {
    serde_json::from_value(json!({
        "job_id": "job-cli-failure", "request_id": "request-cli-failure",
        "client_id": "runner-a", "project_id": "agent:runner-a:project-a",
        "kind": "shell", "shell": "direct_argv", "command_preview": "cli task",
        "status": "failed", "created_at": 100, "started_at": 101, "ended_at": 102,
        "exit_code": 1, "command_execution_state": "completed", "last_update_seq": 2,
        "structured_execution": {
            "execution_source": "run_process", "arg_count": 2, "stdin_present": false
        }
    }))
    .unwrap()
}

fn evaluate(job: &ShellJobInfo, failure: CliFailureReason) -> CliFallbackEligibility {
    cli_fallback_eligibility(job, failure, CliCancellationOrigin::None)
}

#[test]
fn known_provider_failures_require_completed_failed_process_evidence() {
    for reason in [
        CliFailureReason::QuotaExhausted,
        CliFailureReason::RateLimited,
        CliFailureReason::NetworkUnavailable,
        CliFailureReason::ProviderUnavailable,
    ] {
        assert!(evaluate(&failed_job(), reason).is_eligible());
    }
    let mut job = failed_job();
    job.recovered_after_server_restart = true;
    job.recovery_state = Some("reconciled".into());
    job.reconciled_at = Some(105);
    assert!(evaluate(&job, CliFailureReason::QuotaExhausted).is_eligible());
    job.exit_code = Some(-1);
    assert!(evaluate(&job, CliFailureReason::QuotaExhausted).is_eligible());
}

#[test]
fn nonzero_exit_and_untrusted_text_do_not_identify_cli_unavailability() {
    let mut job = failed_job();
    job.error = Some("quota_exhausted; insufficient_quota; network error".into());
    assert_eq!(
        evaluate(&job, CliFailureReason::CommandFailed),
        rejected(CliFallbackBlockReason::FailureNotRecoverable)
    );
    job.exit_code = Some(0);
    assert!(!evaluate(&job, CliFailureReason::QuotaExhausted).is_eligible());
    job.status = "completed".into();
    assert_eq!(
        evaluate(&job, CliFailureReason::QuotaExhausted),
        rejected(CliFallbackBlockReason::SuccessfulExecution)
    );
}

#[test]
fn timeout_requires_runner_cleanup_evidence_and_known_availability_failure() {
    let mut job = failed_job();
    job.command_execution_state = Some(ShellCommandExecutionState::TimedOut);
    job.exit_code = Some(-1);
    job.error = Some("command timed out".into());
    for status in ["timeout", "timed_out"] {
        job.status = status.into();
        assert!(evaluate(&job, CliFailureReason::DeadlineExceeded).is_eligible());
        assert!(evaluate(&job, CliFailureReason::NoResponse).is_eligible());
        // A known provider error can precede a CLI hanging until its deadline.
        assert!(evaluate(&job, CliFailureReason::QuotaExhausted).is_eligible());
        assert!(!evaluate(&job, CliFailureReason::StartupFailed).is_eligible());
    }
    job.command_execution_state = Some(ShellCommandExecutionState::OutcomeUnknown);
    assert_eq!(
        evaluate(&job, CliFailureReason::DeadlineExceeded),
        waiting(CliFallbackBlockReason::ExecutionUnresolved)
    );
}

#[test]
fn stopped_job_requires_coordinator_cancellation_and_confirmed_completion() {
    let mut job = failed_job();
    job.status = "stopped".into();
    job.exit_code = Some(-1);
    job.error = Some("job stopped".into());
    for origin in [
        CliCancellationOrigin::None,
        CliCancellationOrigin::User,
        CliCancellationOrigin::Unknown,
    ] {
        assert!(
            !cli_fallback_eligibility(&job, CliFailureReason::NoResponse, origin).is_eligible()
        );
    }
    assert!(cli_fallback_eligibility(
        &job,
        CliFailureReason::NoResponse,
        CliCancellationOrigin::Coordinator
    )
    .is_eligible());
    job.command_execution_state = Some(ShellCommandExecutionState::OutcomeUnknown);
    assert!(!cli_fallback_eligibility(
        &job,
        CliFailureReason::NoResponse,
        CliCancellationOrigin::Coordinator
    )
    .is_eligible());
}

#[test]
fn user_cancel_and_permission_or_isolation_denials_never_enable_fallback() {
    for reason in [
        CliFailureReason::UserCancelled,
        CliFailureReason::PermissionDenied,
        CliFailureReason::IsolationUnavailable,
        CliFailureReason::DiscoveryOnly,
    ] {
        assert_eq!(
            evaluate(&failed_job(), reason),
            rejected(CliFallbackBlockReason::FailureNotRecoverable)
        );
    }
    assert!(!cli_fallback_eligibility(
        &failed_job(),
        CliFailureReason::QuotaExhausted,
        CliCancellationOrigin::User
    )
    .is_eligible());
}

#[test]
fn startup_failure_requires_authoritative_pre_spawn_failure() {
    let mut job = failed_job();
    job.started_at = None;
    job.exit_code = None;
    job.command_execution_state = Some(ShellCommandExecutionState::NotStarted);
    job.error = Some("failed to spawn structured process".into());
    assert!(evaluate(&job, CliFailureReason::StartupFailed).is_eligible());
    job.status = "lost".into();
    assert_eq!(
        evaluate(&job, CliFailureReason::StartupFailed),
        waiting(CliFallbackBlockReason::ExecutionUnresolved)
    );
}

#[test]
fn active_or_recovering_jobs_never_authorize_replacement() {
    for status in [
        "queued",
        "agent_queued",
        "started",
        "running",
        "stop_requested",
    ] {
        let mut job = failed_job();
        job.status = status.into();
        assert_eq!(
            evaluate(&job, CliFailureReason::NoResponse),
            waiting(CliFallbackBlockReason::ExecutionActive)
        );
    }
    for status in ["lost", "recovering"] {
        let mut job = failed_job();
        job.status = status.into();
        assert_eq!(
            evaluate(&job, CliFailureReason::NoResponse),
            waiting(CliFallbackBlockReason::ExecutionUnresolved)
        );
    }
    let mut job = failed_job();
    job.recovery_state = Some("lost_after_reconcile".into());
    assert!(!evaluate(&job, CliFailureReason::QuotaExhausted).is_eligible());
}

#[test]
fn unsupported_execution_paths_and_incomplete_evidence_fail_closed() {
    for source in [
        "run_script",
        "run_detached_process",
        "coding_agent_start",
        "discovery",
    ] {
        let mut job = failed_job();
        job.structured_execution.as_mut().unwrap().execution_source = source.into();
        assert!(!evaluate(&job, CliFailureReason::QuotaExhausted).is_eligible());
    }
    let mutations: [fn(&mut ShellJobInfo); 13] = [
        |job| job.structured_execution = None,
        |job| job.command_execution_state = None,
        |job| job.project_id = None,
        |job| job.request_id = None,
        |job| job.client_id.clear(),
        |job| job.job_id.clear(),
        |job| job.shell = None,
        |job| job.ssh_resource = Some("remote".into()),
        |job| job.ended_at = None,
        |job| job.started_at = None,
        |job| job.last_update_seq = None,
        |job| job.exit_code = None,
        |job| job.structured_execution.as_mut().unwrap().script_bytes = Some(10),
    ];
    for mutate in mutations {
        let mut job = failed_job();
        mutate(&mut job);
        assert!(!evaluate(&job, CliFailureReason::QuotaExhausted).is_eligible());
    }
}

#[test]
fn mismatched_lifecycle_and_timestamps_never_prove_safe_failure() {
    let mutations: [fn(&mut ShellJobInfo); 6] = [
        |job| job.status = "cancelled".into(),
        |job| job.status = "unknown".into(),
        |job| job.command_execution_state = Some(ShellCommandExecutionState::TimedOut),
        |job| job.started_at = Some(103),
        |job| job.ended_at = Some(99),
        |job| job.last_update_seq = Some(0),
    ];
    for mutate in mutations {
        let mut job = failed_job();
        mutate(&mut job);
        assert!(!evaluate(&job, CliFailureReason::QuotaExhausted).is_eligible());
    }
}

#[test]
fn serialized_contract_is_explicit_and_unknown_variants_are_rejected() {
    assert_eq!(
        serde_json::to_value(evaluate(&failed_job(), CliFailureReason::QuotaExhausted)).unwrap(),
        json!({"state": "eligible"})
    );
    assert_eq!(
        serde_json::to_value(waiting(CliFallbackBlockReason::ExecutionUnresolved)).unwrap(),
        json!({"state": "waiting", "reason": "execution_unresolved"})
    );
    assert!(serde_json::from_value::<CliFailureReason>(json!("unknown")).is_err());
    assert!(serde_json::from_value::<CliFallbackEligibility>(json!({
        "state": "eligible", "bypass": true
    }))
    .is_err());
}
