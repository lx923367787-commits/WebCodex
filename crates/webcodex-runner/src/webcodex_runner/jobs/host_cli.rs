//! Execution resources derive from the same registry used by CLI discovery.
use super::*;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCliWorker {
    pub username: String,
    pub sid: String,
    pub password_file: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCliConfig {
    #[serde(skip)]
    pub load_errors: BTreeMap<String, String>,
    #[serde(default)]
    pub worker: Option<NativeCliWorker>,
    #[serde(default)]
    pub providers: Vec<String>,
}

pub(crate) fn expand(config: &mut AgentExecutionResourcesConfig) -> Result<(), String> {
    if !cfg!(windows) {
        return Ok(());
    }
    let mut failures = BTreeMap::new();
    let discovered = (|| {
        Ok::<_, String>((
            webcodex_process::cli_registry::configured_cli_adapters()?,
            webcodex_process::cli_registry::discover_cli_installations(&[])?,
        ))
    })();
    match discovered {
        Err(error) => {
            failures.insert("*".into(), error);
        }
        Ok((adapters, installed)) => {
            for installation in installed.into_iter().filter(|entry| entry.native) {
                let Some(adapter) = adapters
                    .iter()
                    .find(|a| a.id == installation.provider_id && a.invocation.is_some())
                    .cloned()
                else {
                    continue;
                };
                let id = installation.provider_id.clone();
                let loaded = (|| {
                    let provider = AgentCliProvider::parse(&adapter.id)?;
                    let pinned =
                        pin_discovered_catalog_executable(&installation.executable, provider)?;
                    let digest = pinned
                        .descriptor
                        .executable_identity
                        .strip_prefix("sha256:")
                        .ok_or("native_cli_identity_invalid")?
                        .to_owned();
                    let host = webcodex_core::host_cli::HostCliRegistration {
                        adapter,
                        executable_sha256: digest.clone(),
                        worker_sid: String::new(),
                    };
                    host.validate()?;
                    Ok::<_, String>(AgentExecutionResourceProfile {
                        id: format!("native-{id}"),
                        provider: Some(provider),
                        host_cli: Some(host),
                        native_worker: None,
                        reviewed_controls: None,
                        user_https_proxy: false,
                        codebuddy_config_directory: None,
                        executable: pinned.program,
                        executable_sha256: digest,
                        files: vec![],
                    })
                })();
                match loaded {
                    Ok(profile) => {
                        config.profiles.retain(|p| p.provider != profile.provider);
                        config.profiles.push(profile);
                    }
                    Err(error) => {
                        failures.insert(id, error);
                    }
                }
            }
        }
    }
    for (provider, error) in &failures {
        tracing::warn!(provider = %provider, error = %error, "optional CLI resource load failed");
    }
    config
        .native_cli
        .get_or_insert_with(NativeCliConfig::default)
        .load_errors = failures;
    Ok(())
}
