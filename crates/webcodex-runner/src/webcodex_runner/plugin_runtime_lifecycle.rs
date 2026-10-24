//! Private lifecycle for the configured chat-agents process. Never a Tool.
use super::*;

const VERSION: &str = "webcodex-chat-agents-lifecycle-v1";
const METHOD: &str = "webcodex/chat-agents/lifecycle";
const PROVIDER: &str = "chat-agents";
const BUDGET: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeState {
    Passive,
    Activating,
    Active,
    Stopping,
    Stopped,
    Unknown,
}

#[derive(Default)]
pub(super) struct ProcessCustody {
    pub(super) child: Option<ManagedChild>,
    pub(super) native_exit_confirmed: bool,
}

pub(super) enum NativeStopObservation {
    MissingChild,
    Observed {
        termination_succeeded: bool,
        tree_empty: bool,
        leader_exited: bool,
    },
}

pub(super) fn confirms_native_stop(observation: NativeStopObservation) -> bool {
    matches!(
        observation,
        NativeStopObservation::Observed {
            termination_succeeded: true,
            tree_empty: true,
            leader_exited: true
        }
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Capability {
    protocol_version: String,
    provider_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LifecycleResult {
    protocol_version: String,
    provider_id: String,
    provider_instance_id: String,
    state: String,
}

pub(super) fn validate_capability(result: &Value, required: bool) -> Result<(), ProviderFailure> {
    if !required {
        return Ok(());
    }
    let capability = result
        .get("runtimeLifecycle")
        .cloned()
        .and_then(|value| serde_json::from_value::<Capability>(value).ok());
    if capability
        .is_some_and(|value| value.protocol_version == VERSION && value.provider_id == PROVIDER)
    {
        Ok(())
    } else {
        Err(ProviderFailure::not_started(
            "plugin_runtime_capability_missing",
        ))
    }
}

impl ProviderEntry {
    pub(super) fn runtime_allows_business(&self) -> bool {
        self.runtime.as_ref().is_none_or(|state| {
            state
                .lock()
                .map(|value| *value == RuntimeState::Active)
                .unwrap_or(false)
        })
    }

    fn runtime_deadline(&self) -> Instant {
        Instant::now() + self.timeout.min(BUDGET)
    }

    fn runtime_request(&self, action: &str, deadline: Instant) -> Result<(), ProviderFailure> {
        let mut session = loop {
            if Instant::now() >= deadline {
                return Err(ProviderFailure::before_send_timeout());
            }
            match self.session.try_lock() {
                Ok(session) => break session,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(ProviderFailure::not_started("plugin_provider_state_failed"))
                }
            }
        };
        let connection = session
            .as_mut()
            .ok_or_else(|| ProviderFailure::not_started("plugin_provider_unavailable"))?;
        if action == "activate" {
            let permitted = self.runtime.as_ref().is_some_and(|state| {
                state
                    .lock()
                    .map(|value| *value == RuntimeState::Activating)
                    .unwrap_or(false)
            });
            if !permitted || connection.stopping.load(Ordering::SeqCst) {
                return Err(ProviderFailure::not_started("plugin_manager_stopping"));
            }
        }
        let result = connection.request_until(
            METHOD,
            json!({"protocolVersion": VERSION, "providerId": PROVIDER,
                "providerInstanceId": self.instance_id, "action": action}),
            deadline,
            true,
            action == "deactivate",
        )?;
        let result: LifecycleResult = serde_json::from_value(result)
            .map_err(|_| runtime_failure("plugin_runtime_response_invalid"))?;
        if result.protocol_version != VERSION
            || result.provider_id != PROVIDER
            || result.provider_instance_id != self.instance_id
            || !matches!(result.state.as_str(), "active" | "stopped" | "unknown")
        {
            return Err(runtime_failure("plugin_runtime_response_invalid"));
        }
        let expected = if action == "activate" {
            "active"
        } else {
            "stopped"
        };
        if result.state != expected {
            return Err(runtime_failure("plugin_runtime_outcome_unknown"));
        }
        Ok(())
    }

    pub(super) fn activate_runtime(&self, stopping: &AtomicBool) -> Result<(), ProviderFailure> {
        let Some(state) = &self.runtime else {
            return Ok(());
        };
        let deadline = self.runtime_deadline();
        {
            let mut state = state
                .lock()
                .map_err(|_| ProviderFailure::not_started("plugin_provider_state_failed"))?;
            if self.failed.load(Ordering::SeqCst) {
                return Err(ProviderFailure::not_started("plugin_provider_unavailable"));
            }
            if *state == RuntimeState::Active && !stopping.load(Ordering::SeqCst) {
                return Ok(());
            }
            if *state != RuntimeState::Passive
                || self.failed.load(Ordering::SeqCst)
                || stopping.load(Ordering::SeqCst)
            {
                return Err(ProviderFailure::not_started("plugin_runtime_not_active"));
            }
            *state = RuntimeState::Activating;
        }
        let result = self.runtime_request("activate", deadline);
        {
            let mut state = state
                .lock()
                .map_err(|_| runtime_failure("plugin_provider_state_failed"))?;
            if result.is_ok()
                && *state == RuntimeState::Activating
                && !stopping.load(Ordering::SeqCst)
            {
                *state = RuntimeState::Active;
                return Ok(());
            }
            if !matches!(*state, RuntimeState::Stopping | RuntimeState::Stopped) {
                *state = RuntimeState::Unknown;
            }
        }
        let failure = result
            .err()
            .unwrap_or_else(|| runtime_failure("plugin_manager_stopping"));
        self.retire(failure.code);
        // Never resend an activation after a missing or contradictory receipt.
        self.process.terminate_until(deadline);
        Err(failure)
    }

    pub(super) fn stop_runtime_until(&self, deadline: Instant) -> Result<(), ProviderFailure> {
        let Some(state) = &self.runtime else {
            return Ok(());
        };
        {
            let mut state = state
                .lock()
                .map_err(|_| runtime_failure("plugin_provider_state_failed"))?;
            match *state {
                RuntimeState::Passive | RuntimeState::Stopped => {
                    *state = RuntimeState::Stopped;
                    return Ok(());
                }
                _ => *state = RuntimeState::Stopping,
            }
        }
        let result = self.runtime_request("deactivate", deadline);
        let confirmed = result.is_ok() || self.process.terminate_until(deadline);
        if let Ok(mut state) = state.lock() {
            *state = if confirmed {
                RuntimeState::Stopped
            } else {
                RuntimeState::Unknown
            };
        }
        if confirmed {
            Ok(())
        } else {
            self.retire("plugin_runtime_stop_unconfirmed");
            Err(runtime_failure("plugin_runtime_stop_unconfirmed"))
        }
    }

    pub(super) fn stop_runtime(&self) -> Result<(), ProviderFailure> {
        self.stop_runtime_until(self.runtime_deadline())
    }

    pub(super) fn shutdown_runtime_and_process(&self) {
        let deadline = if self.runtime.is_some() {
            self.runtime_deadline()
        } else {
            Instant::now() + PLUGIN_TERMINATION_BUDGET
        };
        let _ = self.stop_runtime_until(deadline);
        self.process.terminate_until(deadline);
    }
}

fn runtime_failure(code: &'static str) -> ProviderFailure {
    ProviderFailure {
        dispatch_state: PluginDispatchState::OutcomeUnknown,
        code,
        fatal: true,
    }
}
