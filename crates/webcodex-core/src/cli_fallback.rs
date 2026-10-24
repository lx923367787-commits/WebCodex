//! Eligibility for a trusted coordinator to replace one failed CLI execution.
//!
//! This classifies an already authorized, Runner-owned typed `run_process` Job.
//! It does not authenticate a supplied Job, identify an executable as a CLI,
//! reserve capacity, launch a model, or grant access to the source task. Callers
//! must obtain the Job from the authorized registry and bind it to the exact
//! source operation. Failure reasons and cancellation origin come from trusted
//! coordination evidence, never arbitrary stdout, stderr, or a model assertion.

use crate::runner_job_lifecycle::RunnerJobLifecycle;
use crate::runner_protocol::{ShellCommandExecutionState, ShellJobInfo};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliFailureReason {
    QuotaExhausted,
    RateLimited,
    NetworkUnavailable,
    ProviderUnavailable,
    NoResponse,
    DeadlineExceeded,
    StartupFailed,
    CommandFailed,
    PermissionDenied,
    IsolationUnavailable,
    UserCancelled,
    DiscoveryOnly,
}

/// Origin of a cancellation request, not the identity of the request observer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliCancellationOrigin {
    None,
    Coordinator,
    User,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliFallbackBlockReason {
    FailureNotRecoverable,
    CancellationNotCoordinator,
    MissingJobEvidence,
    UnsupportedExecutionSource,
    InconsistentJobEvidence,
    ExecutionUnresolved,
    ExecutionActive,
    SuccessfulExecution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CliFallbackEligibility {
    Eligible {},
    Waiting { reason: CliFallbackBlockReason },
    Rejected { reason: CliFallbackBlockReason },
}

impl CliFallbackEligibility {
    pub const fn is_eligible(self) -> bool {
        matches!(self, Self::Eligible { .. })
    }
}

fn rejected(reason: CliFallbackBlockReason) -> CliFallbackEligibility {
    CliFallbackEligibility::Rejected { reason }
}

fn waiting(reason: CliFallbackBlockReason) -> CliFallbackEligibility {
    CliFallbackEligibility::Waiting { reason }
}

fn nonempty(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

/// Determine whether known execution failure permits requesting a recovery slot.
///
/// Typed process `Completed` and `TimedOut` evidence is emitted only after the
/// Runner confirms whole-tree cleanup, child reaping, and output collection.
/// Missing evidence, `lost`, or a recovery overlay cannot substitute for that
/// confirmation. In particular, a Server-projected lost Job can contain
/// `NotStarted` without proving that the Runner never spawned the process.
pub fn cli_fallback_eligibility(
    job: &ShellJobInfo,
    failure: CliFailureReason,
    cancellation_origin: CliCancellationOrigin,
) -> CliFallbackEligibility {
    use CliFailureReason as Failure;
    use CliFallbackBlockReason as Block;
    use RunnerJobLifecycle as Job;
    use ShellCommandExecutionState as Execution;

    if matches!(
        failure,
        Failure::CommandFailed
            | Failure::PermissionDenied
            | Failure::IsolationUnavailable
            | Failure::UserCancelled
            | Failure::DiscoveryOnly
    ) {
        return rejected(Block::FailureNotRecoverable);
    }
    if matches!(
        cancellation_origin,
        CliCancellationOrigin::User | CliCancellationOrigin::Unknown
    ) {
        return rejected(Block::CancellationNotCoordinator);
    }

    let Some(metadata) = job.structured_execution.as_ref() else {
        return rejected(Block::MissingJobEvidence);
    };
    if metadata.execution_source != "run_process"
        || job.ssh_resource.is_some()
        || job.document.is_some()
        || job.codex.is_some()
        || job.validation.is_some()
        || metadata.validation_tool.is_some()
    {
        return rejected(Block::UnsupportedExecutionSource);
    }
    if !metadata.is_valid() || job.shell.as_deref() != Some("direct_argv") {
        return rejected(Block::InconsistentJobEvidence);
    }
    if !nonempty(&job.job_id)
        || !nonempty(&job.client_id)
        || !job.request_id.as_deref().is_some_and(nonempty)
        || !job.project_id.as_deref().is_some_and(nonempty)
    {
        return rejected(Block::MissingJobEvidence);
    }

    // Neither the public recovery overlay nor a terminal lost lifecycle proves
    // execution ended. Evaluate these before considering NotStarted evidence.
    if job.status == "recovering"
        || job.status == "lost"
        || job
            .recovery_state
            .as_deref()
            .is_some_and(|state| state != "reconciled")
        || job.command_execution_state == Some(Execution::OutcomeUnknown)
    {
        return waiting(Block::ExecutionUnresolved);
    }
    let Ok(status) = Job::from_wire(&job.status) else {
        return rejected(Block::InconsistentJobEvidence);
    };
    if status.is_active() {
        return waiting(Block::ExecutionActive);
    }
    if status == Job::Completed {
        return rejected(Block::SuccessfulExecution);
    }
    if !matches!(
        status,
        Job::Failed | Job::Stopped | Job::Timeout | Job::TimedOut
    ) {
        return rejected(Block::UnsupportedExecutionSource);
    }
    let Some(execution) = job.command_execution_state else {
        return rejected(Block::MissingJobEvidence);
    };
    let Some(ended_at) = job.ended_at else {
        return rejected(Block::MissingJobEvidence);
    };
    if job.last_update_seq.is_none_or(|sequence| sequence == 0)
        || ended_at < job.created_at
        || job
            .started_at
            .is_some_and(|started| started < job.created_at || started > ended_at)
    {
        return rejected(Block::InconsistentJobEvidence);
    }

    let consistent = match (status, execution) {
        (Job::Failed, Execution::NotStarted) => {
            failure == Failure::StartupFailed
                && job.started_at.is_none()
                && job.exit_code.is_none()
                && job.error.as_deref().is_some_and(nonempty)
                && cancellation_origin == CliCancellationOrigin::None
        }
        (Job::Failed, Execution::Completed) => {
            failure != Failure::StartupFailed
                && job.started_at.is_some()
                && job.exit_code.is_some_and(|code| code != 0)
        }
        (Job::Stopped, Execution::Completed) => {
            failure != Failure::StartupFailed
                && cancellation_origin == CliCancellationOrigin::Coordinator
                && job.started_at.is_some()
                && job.exit_code == Some(-1)
                && job.error.as_deref().is_some_and(nonempty)
        }
        (Job::Timeout | Job::TimedOut, Execution::TimedOut) => {
            failure != Failure::StartupFailed
                && job.started_at.is_some()
                && job.exit_code == Some(-1)
                && job.error.as_deref().is_some_and(nonempty)
        }
        _ => false,
    };
    if consistent {
        CliFallbackEligibility::Eligible {}
    } else {
        rejected(Block::InconsistentJobEvidence)
    }
}

#[cfg(test)]
#[path = "cli_fallback_tests.rs"]
mod tests;
