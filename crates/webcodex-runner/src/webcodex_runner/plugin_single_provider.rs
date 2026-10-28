//! Prepare one ordinary Plugin before saving its connection configuration.
//! The candidate gate is held until commit or discard, including the caller's
//! disk write. Unchanged providers and already-running calls keep their process.

use super::*;
use std::sync::MutexGuard;

pub(crate) struct PreparedPluginProvider<'manager> {
    manager: &'manager PluginManager,
    provider: Arc<ProviderEntry>,
    changed: bool,
    // Drop an abandoned candidate process before releasing management custody.
    _candidate_guard: MutexGuard<'manager, ()>,
}

impl PreparedPluginProvider<'_> {
    pub(crate) fn tool_count(&self) -> usize {
        self.provider
            .catalog
            .get()
            .map_or(0, |catalog| catalog.tools().len())
    }

    pub(crate) fn changed(&self) -> bool {
        self.changed
    }
}

pub(crate) struct PreparedPluginRemoval<'manager> {
    manager: &'manager PluginManager,
    _candidate_guard: MutexGuard<'manager, ()>,
    provider_id: String,
    changed: bool,
}

impl PreparedPluginRemoval<'_> {
    pub(crate) fn changed(&self) -> bool {
        self.changed
    }
}

impl PluginManager {
    /// The caller supplies its already validated Runner configuration and keeps
    /// its configuration-write lock until commit (or candidate discard).
    pub(crate) fn prepare(
        &self,
        config: &RunnerConfig,
        provider_id: &str,
    ) -> Result<PreparedPluginProvider<'_>, String> {
        self.prepare_single(config, provider_id, false)
    }

    /// Explicit code update/reconnect. A failed business call is never replayed.
    pub(crate) fn prepare_reconnect(
        &self,
        config: &RunnerConfig,
        provider_id: &str,
    ) -> Result<PreparedPluginProvider<'_>, String> {
        self.prepare_single(config, provider_id, true)
    }

    fn single_candidate_guard(&self) -> Result<MutexGuard<'_, ()>, String> {
        let guard = self
            .candidate_gate
            .try_lock()
            .map_err(|error| match error {
                TryLockError::WouldBlock => "plugin_reload_busy".to_string(),
                TryLockError::Poisoned(_) => "plugin_reload_state_failed".to_string(),
            })?;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("plugin_manager_stopping".into());
        }
        Ok(guard)
    }

    fn prepare_single(
        &self,
        config: &RunnerConfig,
        provider_id: &str,
        reconnect: bool,
    ) -> Result<PreparedPluginProvider<'_>, String> {
        let candidate_guard = self.single_candidate_guard()?;
        self.prepare_single_locked(config, provider_id, reconnect, candidate_guard)
    }

    fn prepare_single_locked<'manager>(
        &'manager self,
        config: &RunnerConfig,
        provider_id: &str,
        reconnect: bool,
        candidate_guard: MutexGuard<'manager, ()>,
    ) -> Result<PreparedPluginProvider<'manager>, String> {
        let provider_config = config
            .plugins
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .ok_or_else(|| "plugin_not_configured".to_string())?;
        // Managed chat runtimes have a separate activation/physical-stop
        // contract. The ordinary connector operation must not take it over.
        if provider_config.runtime_lifecycle.is_some() {
            return Err("plugin_runtime_lifecycle_managed".into());
        }
        let existing = self
            .committed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .providers
            .get(provider_id)
            .cloned();
        if existing
            .as_ref()
            .is_some_and(|provider| provider.runtime.is_some())
        {
            return Err("plugin_runtime_lifecycle_managed".into());
        }
        let environment = PluginEnvironmentSnapshot::for_provider(&config.shell, provider_config);
        let timeout = Duration::from_secs(
            provider_config
                .timeout_secs
                .unwrap_or(config.plugins.request_timeout_secs),
        );
        if let Some(provider) = existing.filter(|provider| {
            !reconnect
                && !provider.failed.load(Ordering::SeqCst)
                && provider.config == *provider_config
                && provider.environment == environment
                && provider.timeout == timeout
        }) {
            return Ok(PreparedPluginProvider {
                manager: self,
                _candidate_guard: candidate_guard,
                provider,
                changed: false,
            });
        }
        let generation = self.next_generation.fetch_add(1, Ordering::SeqCst);
        let (provider, _, failure) = prepare_provider(
            provider_config,
            &config.shell,
            generation,
            Duration::from_secs(config.plugins.request_timeout_secs),
            &self.prepared_profiles,
            &self.stopping,
        );
        if let Some(failure) = failure {
            provider.shutdown();
            return Err(failure.code.into());
        }
        if self.stopping.load(Ordering::SeqCst) {
            provider.shutdown();
            return Err("plugin_manager_stopping".into());
        }
        Ok(PreparedPluginProvider {
            manager: self,
            _candidate_guard: candidate_guard,
            provider,
            changed: true,
        })
    }

    /// A later discovery request may restore a failed ordinary connection from
    /// its saved configuration. This never runs or replays a business call.
    pub(super) fn recover_failed_provider(
        &self,
        provider_id: &str,
        expected_instance: &str,
    ) -> Result<(), String> {
        let candidate_guard = self.single_candidate_guard()?;
        let Some(provider) = self.resolve_provider(provider_id, expected_instance) else {
            return Ok(()); // Another explicit operation already replaced it.
        };
        if !provider.failed.load(Ordering::SeqCst) || provider.runtime.is_some() {
            return Err("plugin_provider_unavailable".into());
        }
        // Read while holding the same gate as prepare/save/commit, so recovery
        // cannot resurrect a connection concurrently removed by its owner.
        let config = load_config(&self.config_path)
            .map_err(|error| plugin_config_error_code(&error).to_string())?;
        let prepared = self.prepare_single_locked(&config, provider_id, false, candidate_guard)?;
        self.commit(prepared);
        Ok(())
    }

    /// No fallible admission remains after the caller saves the configuration.
    pub(crate) fn commit(&self, prepared: PreparedPluginProvider<'_>) -> PluginProviderView {
        assert!(
            std::ptr::eq(self, prepared.manager),
            "candidate belongs to another Plugin manager"
        );
        if !prepared.changed {
            return prepared.provider.view();
        }
        let provider = &prepared.provider;
        let retired = {
            let mut committed = self.committed.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(config) = committed
                .config
                .providers
                .iter_mut()
                .find(|config| config.id == provider.config.id)
            {
                *config = provider.config.clone();
            } else {
                committed.config.providers.push(provider.config.clone());
            }
            let retired = committed
                .providers
                .insert(provider.config.id.clone(), Arc::clone(provider));
            if let Some(old) = &retired {
                old.retire("plugin_provider_replaced");
            }
            retired
        };
        // The manager's map releases its old Arc. An in-flight operation owns
        // another Arc until it completes; only then does Drop stop that process.
        drop(retired);
        if self.stopping.load(Ordering::SeqCst) {
            provider.retire("plugin_manager_stopping");
            provider.shutdown();
        }
        provider.view()
    }

    pub(crate) fn prepare_remove(
        &self,
        provider_id: &str,
    ) -> Result<PreparedPluginRemoval<'_>, String> {
        let candidate_guard = self.single_candidate_guard()?;
        let committed = self.committed.lock().unwrap_or_else(|p| p.into_inner());
        let existing = committed.providers.get(provider_id);
        if existing.is_some_and(|provider| provider.runtime.is_some()) {
            return Err("plugin_runtime_lifecycle_managed".into());
        }
        Ok(PreparedPluginRemoval {
            manager: self,
            _candidate_guard: candidate_guard,
            provider_id: provider_id.into(),
            changed: existing.is_some(),
        })
    }

    pub(crate) fn commit_remove(&self, prepared: PreparedPluginRemoval<'_>) -> bool {
        assert!(
            std::ptr::eq(self, prepared.manager),
            "candidate belongs to another Plugin manager"
        );
        let retired = {
            let mut committed = self.committed.lock().unwrap_or_else(|p| p.into_inner());
            committed
                .config
                .providers
                .retain(|config| config.id != prepared.provider_id);
            let retired = committed.providers.remove(&prepared.provider_id);
            if let Some(provider) = &retired {
                provider.retire("plugin_provider_removed");
            }
            retired
        };
        let changed = retired.is_some();
        drop(retired);
        changed
    }
}
