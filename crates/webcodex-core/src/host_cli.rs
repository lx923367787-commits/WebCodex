//! Explicit operator-owned host CLI registration. This grants no Agent identity
//! or access to another Agent's memory/goal APIs. Host execution shares the OS
//! Runner's existing filesystem permissions. Legacy worker identities are ignored.
use crate::cli_adapter::CliAdapter;
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "host_cli_tests.rs"]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCliRegistration {
    pub adapter: CliAdapter,
    pub executable_sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub worker_sid: String,
}

impl HostCliRegistration {
    pub fn validate(&self) -> Result<(), String> {
        self.adapter.validate()?;
        if self.adapter.invocation.is_none()
            || self.executable_sha256.len() != 64
            || !self
                .executable_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("host_cli_registration_invalid".into());
        }
        Ok(())
    }

    pub fn manifest(&self, resource_id: &str) -> Result<String, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(&(
            "webcodex.host-cli.v1",
            resource_id,
            &self.adapter,
            &self.executable_sha256,
        ))
        .map_err(|_| "host_cli_manifest_encoding")?;
        Ok(crate::agent_cli::sha256(&bytes))
    }

    /// Preserve normal native flags. The CLI itself validates supported options;
    /// the framework only checks the common process wire-format bounds.
    pub fn validate_args(&self, args: &[String]) -> Result<(), String> {
        self.validate()?;
        crate::runner_protocol::validate_process_argv(&crate::runner_protocol::ShellProcessArgv {
            executable: format!("sha256:{}", self.executable_sha256),
            args: args.to_vec(),
        })
    }
}
