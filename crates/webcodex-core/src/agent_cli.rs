//! Fixed metadata and explicit delivery contracts. Shape checks grant no authority.
use crate::runner_protocol::{
    AgentExecutionBinding, AgentExecutionResourceDescriptor, AgentExecutionTarget,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const AGENT_CLI_CATALOG_KIND: &str = "query_agent_cli_catalog";
pub const AGENT_CLI_INVENTORY_KIND: &str = "query_agent_cli_inventory";
pub const AGENT_CLI_DELIVERY_KIND: &str = "read_agent_cli_delivery_snapshot";
pub const AGENT_CLI_CONTROL_TIMEOUT_SECS: u64 = 40;
pub const AGENT_CLI_CATALOG_TTL_MS: i64 = 60_000;
pub const AGENT_CLI_CONTROL_REQUEST_MAX_BYTES: usize = 32 * 1024;
pub const AGENT_CLI_CATALOG_MAX_BYTES: usize = 128 * 1024;
pub const AGENT_CLI_DELIVERY_MAX_BYTES: usize = 1024 * 1024;
/// The immutable artifact limit excludes the transport's binding/hash wrapper.
pub const AGENT_CLI_DELIVERY_RESPONSE_MAX_BYTES: usize =
    AGENT_CLI_DELIVERY_MAX_BYTES + AGENT_CLI_CONTROL_REQUEST_MAX_BYTES;
pub const AGENT_CLI_MANIFEST_MAX_BYTES: usize = 32 * 1024;
pub const AGENT_CLI_DELIVERY_SUMMARY_MAX_BYTES: usize = 16 * 1024;
pub const AGENT_CLI_DELIVERY_FILE_MAX_BYTES: usize = 256 * 1024;
pub const AGENT_CLI_DELIVERY_TEXT_MAX_BYTES: usize = 512 * 1024;
pub const AGENT_CLI_DELIVERY_MAX_FILES: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentCliInventoryQuery {
    pub version: u32,
    #[serde(default)]
    pub native_only: bool,
    pub target: AgentExecutionTarget,
    pub deadline_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentCliProvider>,
}
impl AgentCliInventoryQuery {
    pub fn validate(&self) -> Result<(), String> {
        version(self.version)?;
        self.target.validate()?;
        if self.deadline_at_ms <= 0 {
            return Err("agent_cli_invalid_deadline".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentCliInstallation {
    pub provider_id: String,
    pub command: String,
    pub label: String,
    pub installed: bool,
    pub native: bool,
    pub adapter_available: bool,
    pub status: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentCliInventory {
    pub version: u32,
    pub target: AgentExecutionTarget,
    pub observed_at_ms: i64,
    pub expires_at_ms: i64,
    pub installations: Vec<AgentCliInstallation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<AgentCliCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentCliCapabilities {
    /// Internal Server/Runner identity; omitted from the model projection.
    pub executable_identity: String,
    pub provider_id: String,
    pub label: String,
    pub installed: bool,
    pub version: String,
    pub models: Vec<String>,
    pub efforts: Vec<String>,
    pub source: String,
    pub status: String,
    pub message: String,
    pub account_availability_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<crate::cli_adapter::CliAdapter>,
}
impl AgentCliInventory {
    pub fn validate_for(&self, query: &AgentCliInventoryQuery, now: i64) -> Result<(), String> {
        query.validate()?;
        version(self.version)?;
        if self.target != query.target
            || self.observed_at_ms <= 0
            || self.observed_at_ms > now
            || self.expires_at_ms <= now
            || self.expires_at_ms - self.observed_at_ms != AGENT_CLI_CATALOG_TTL_MS
            || self.installations.len() > crate::cli_adapter::MAX_CLI_ADAPTERS
        {
            return Err("agent_cli_inventory_invalid".into());
        }
        let mut ids = BTreeSet::new();
        for entry in &self.installations {
            if !crate::cli_adapter::valid_cli_id(&entry.provider_id)
                || !ids.insert(&entry.provider_id)
                || !matches!(
                    entry.status.as_str(),
                    "discovered" | "adapter_required" | "launcher_requires_adapter" | "unavailable"
                )
            {
                return Err("agent_cli_inventory_invalid".into());
            }
            text(&entry.label, 128, false)?;
            text(&entry.command, 4096, !entry.installed)?;
            text(&entry.message, 2048, true)?;
        }
        if let Some(capabilities) = &self.capabilities {
            if let Some(adapter) = &capabilities.adapter {
                adapter.validate()?;
                if adapter.id != capabilities.provider_id {
                    return Err("agent_cli_capabilities_invalid".into());
                }
            }
            let executable_hash = capabilities
                .executable_identity
                .strip_prefix("sha256:")
                .ok_or("agent_cli_capabilities_invalid")?;
            digest(executable_hash)?;
            if query.provider.as_ref().map(AgentCliProvider::id)
                != Some(capabilities.provider_id.as_str())
                || (capabilities.installed
                    && !self.installations.iter().any(|entry| {
                        entry.installed && entry.provider_id == capabilities.provider_id
                    }))
                || capabilities.account_availability_verified
                || capabilities.models.len() > 256
                || capabilities.efforts.len() > 32
                || !matches!(
                    capabilities.source.as_str(),
                    "installed_help" | "live_catalog" | "unavailable"
                )
                || !matches!(
                    capabilities.status.as_str(),
                    "ready" | "error" | "unavailable"
                )
                || (capabilities.status == "ready"
                    && (!capabilities.installed || capabilities.models.is_empty()))
            {
                return Err("agent_cli_capabilities_invalid".into());
            }
            text(&capabilities.label, 128, false)?;
            text(&capabilities.version, 128, capabilities.status != "ready")?;
            text(&capabilities.message, 2048, true)?;
            for (options, max) in [(&capabilities.models, 128), (&capabilities.efforts, 64)] {
                let mut seen = BTreeSet::new();
                for option in options {
                    text(option, max, false)?;
                    if !seen.insert(option) {
                        return Err("agent_cli_capabilities_invalid".into());
                    }
                }
            }
        } else if query.provider.is_some() {
            return Err("agent_cli_capabilities_missing".into());
        }
        Ok(())
    }
}

/// Bounded opaque registry ID. Kept Copy so exact resource fences retain value
/// semantics without baking a list of products into the wire protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentCliProvider {
    bytes: [u8; 64],
    len: u8,
}
impl AgentCliProvider {
    #[allow(non_upper_case_globals)]
    pub const WorkBuddy: Self = Self::constant("workbuddy");
    #[allow(non_upper_case_globals)]
    pub const Antigravity: Self = Self::constant("antigravity");
    const fn constant(id: &str) -> Self {
        let mut bytes = [0; 64];
        let mut index = 0;
        while index < id.len() {
            bytes[index] = id.as_bytes()[index];
            index += 1;
        }
        Self {
            bytes,
            len: id.len() as u8,
        }
    }
    pub fn parse(id: &str) -> Result<Self, String> {
        if !crate::cli_adapter::valid_cli_id(id) {
            return Err("agent_cli_invalid_provider_id".into());
        }
        Ok(Self::constant(id))
    }
    pub fn id(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).expect("validated ASCII ID")
    }
}
impl Serialize for AgentCliProvider {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}
impl<'de> Deserialize<'de> for AgentCliProvider {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::parse(&id).map_err(serde::de::Error::custom)
    }
}

fn version(value: u32) -> Result<(), String> {
    if value == 1 {
        Ok(())
    } else {
        Err("agent_cli_unsupported_version".into())
    }
}
fn text(value: &str, max: usize, empty: bool) -> Result<(), String> {
    if (!empty && value.trim().is_empty())
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        Err("agent_cli_invalid_text".into())
    } else {
        Ok(())
    }
}
fn digest(value: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err("agent_cli_invalid_sha256".into())
    }
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliCatalogQuery {
    pub version: u32,
    pub target: AgentExecutionTarget,
    pub provider: AgentCliProvider,
    /// Generated by the trusted Registry; never a model-supplied time budget.
    pub deadline_at_ms: i64,
}
impl AgentCliCatalogQuery {
    pub fn validate(&self) -> Result<(), String> {
        version(self.version)?;
        self.target.validate()?;
        if self.deadline_at_ms <= 0 {
            return Err("agent_cli_invalid_deadline".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliCatalog {
    pub version: u32,
    pub target: AgentExecutionTarget,
    pub resource_set_id: String,
    pub resource_manifest_hash: String,
    pub executable_identity: String,
    pub provider: AgentCliProvider,
    pub cli_version: String,
    pub models: Vec<String>,
    pub efforts: Vec<String>,
    pub source: String,
    pub status: String,
    pub message: String,
    pub observed_at_ms: i64,
    pub expires_at_ms: i64,
    /// Fixed metadata probes never verify model entitlement or nested execution.
    pub account_availability_verified: bool,
    pub nested_delegation_verified_disabled: bool,
}
impl AgentCliCatalog {
    pub fn validate_for(
        &self,
        query: &AgentCliCatalogQuery,
        resource: &AgentExecutionResourceDescriptor,
    ) -> Result<(), String> {
        query.validate()?;
        resource.validate()?;
        version(self.version)?;
        if self.target != query.target
            || self.provider != query.provider
            || resource.provider != Some(query.provider)
            || self.resource_set_id != resource.resource_set_id
            || self.resource_manifest_hash != resource.manifest_hash
            || self.executable_identity != resource.executable_identity
        {
            return Err("agent_cli_catalog_identity_mismatch".into());
        }
        if self.account_availability_verified || self.nested_delegation_verified_disabled {
            return Err("agent_cli_metadata_is_not_execution_verification".into());
        }
        if self.observed_at_ms <= 0
            || self.expires_at_ms.checked_sub(self.observed_at_ms) != Some(AGENT_CLI_CATALOG_TTL_MS)
        {
            return Err("agent_cli_invalid_catalog_expiry".into());
        }
        if !matches!(
            self.source.as_str(),
            "installed_help" | "live_catalog" | "unavailable"
        ) || !matches!(self.status.as_str(), "ready" | "error" | "unavailable")
        {
            return Err("agent_cli_invalid_catalog_state".into());
        }
        text(&self.cli_version, 128, self.status != "ready")?;
        text(&self.message, 2048, true)?;
        if self.models.len() > 256 || self.efforts.len() > 32 {
            return Err("agent_cli_catalog_limit".into());
        }
        for (values, limit) in [(&self.models, 128), (&self.efforts, 64)] {
            let mut seen = BTreeSet::new();
            for value in values {
                text(value, limit, false)?;
                if !seen.insert(value) {
                    return Err("agent_cli_catalog_duplicate".into());
                }
            }
        }
        if self.status == "ready" && (self.models.is_empty() || self.source == "unavailable") {
            return Err("agent_cli_catalog_not_ready".into());
        }
        let selected_adapter = resource
            .reviewed_controls
            .as_ref()
            .and_then(|record| record.adapter.clone())
            .or_else(|| {
                crate::cli_adapter::cli_adapters(&[])
                    .ok()?
                    .into_iter()
                    .find(|adapter| adapter.id == self.provider.id())
            });
        let expected_source = match selected_adapter.as_ref().map(|adapter| adapter.metadata) {
            Some(crate::cli_adapter::CliMetadataRecipe::AntigravityModels) => "live_catalog",
            _ => "installed_help",
        };
        if self.source != "unavailable" && self.source != expected_source {
            return Err("agent_cli_catalog_source_mismatch".into());
        }
        if serde_json::to_vec(self)
            .map_err(|_| "agent_cli_encoding_failed")?
            .len()
            > AGENT_CLI_CATALOG_MAX_BYTES
        {
            return Err("agent_cli_catalog_limit".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliDeliveryQuery {
    pub version: u32,
    /// Current authenticated read transport, distinct from the immutable past
    /// execution target. Same client only; this never authorizes a new start.
    pub target: AgentExecutionTarget,
    pub binding: AgentExecutionBinding,
    pub deadline_at_ms: i64,
}
impl AgentCliDeliveryQuery {
    pub fn validate(&self) -> Result<(), String> {
        version(self.version)?;
        self.target.validate()?;
        self.binding.validate()?;
        if self.target.client_id != self.binding.target.client_id {
            return Err("agent_cli_delivery_reader_mismatch".into());
        }
        if !matches!(
            self.binding.carrier,
            crate::runner_protocol::AgentExecutionCarrier::Job { .. }
        ) {
            return Err("agent_cli_delivery_job_required".into());
        }
        if self.deadline_at_ms <= 0 {
            return Err("agent_cli_invalid_deadline".into());
        }
        Ok(())
    }
}

/// Relative to workspace/publish. Filesystem authority still requires safe
/// handle traversal, non-reparse/single-link checks and an actual stopped Job.
pub fn validate_delivery_path(path: &str) -> Result<(), String> {
    if path.len() > 256 || !path.starts_with("files/") || path.contains('\\') {
        return Err("agent_cli_delivery_invalid_path".into());
    }
    for part in path.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        {
            return Err("agent_cli_delivery_invalid_path".into());
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let device_suffix = stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"));
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || matches!(
                device_suffix,
                Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            )
        {
            return Err("agent_cli_delivery_invalid_path".into());
        }
    }
    if path == "files/" {
        return Err("agent_cli_delivery_invalid_path".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliDeliveryManifestFile {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliDeliveryManifest {
    pub version: u32,
    pub summary: String,
    pub files: Vec<AgentCliDeliveryManifestFile>,
}
impl AgentCliDeliveryManifest {
    pub fn validate(&self) -> Result<(), String> {
        version(self.version)?;
        if self.summary.len() > AGENT_CLI_DELIVERY_SUMMARY_MAX_BYTES
            || self.summary.contains('\0')
            || self.files.len() > AGENT_CLI_DELIVERY_MAX_FILES
            || (self.summary.trim().is_empty() && self.files.is_empty())
        {
            return Err("agent_cli_delivery_manifest_limit".into());
        }
        let mut total = self.summary.len() as u64;
        let mut seen = BTreeSet::new();
        for file in &self.files {
            validate_delivery_path(&file.path)?;
            digest(&file.sha256)?;
            if !seen.insert(file.path.to_ascii_lowercase())
                || file.bytes > AGENT_CLI_DELIVERY_FILE_MAX_BYTES as u64
            {
                return Err("agent_cli_delivery_manifest_limit".into());
            }
            total = total
                .checked_add(file.bytes)
                .ok_or("agent_cli_delivery_manifest_limit")?;
        }
        if total > AGENT_CLI_DELIVERY_TEXT_MAX_BYTES as u64
            || serde_json::to_vec(self)
                .map_err(|_| "agent_cli_encoding_failed")?
                .len()
                > AGENT_CLI_MANIFEST_MAX_BYTES
        {
            return Err("agent_cli_delivery_manifest_limit".into());
        }
        Ok(())
    }
}

/// Input v2 declares only explicit publication paths. The stopped collector
/// computes byte lengths and hashes; output artifacts keep the existing v1 DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliDeliveryManifestPath {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliDeliveryManifestV2 {
    pub version: u32,
    pub summary: String,
    pub files: Vec<AgentCliDeliveryManifestPath>,
}

pub struct AgentCliDeliveryDeclaredFile {
    pub path: String,
    pub expected: Option<(u64, String)>,
}
pub struct AgentCliDeliveryDeclaration {
    pub summary: String,
    pub files: Vec<AgentCliDeliveryDeclaredFile>,
}

pub fn parse_delivery_manifest(bytes: &[u8]) -> Result<AgentCliDeliveryDeclaration, String> {
    if bytes.len() > AGENT_CLI_MANIFEST_MAX_BYTES {
        return Err("agent_cli_delivery_manifest_limit".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "agent_cli_delivery_manifest_invalid")?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(1) => {
            let manifest: AgentCliDeliveryManifest =
                serde_json::from_slice(bytes).map_err(|_| "agent_cli_delivery_manifest_invalid")?;
            manifest.validate()?;
            Ok(AgentCliDeliveryDeclaration {
                summary: manifest.summary,
                files: manifest
                    .files
                    .into_iter()
                    .map(|f| AgentCliDeliveryDeclaredFile {
                        path: f.path,
                        expected: Some((f.bytes, f.sha256)),
                    })
                    .collect(),
            })
        }
        Some(2) => {
            let manifest: AgentCliDeliveryManifestV2 =
                serde_json::from_slice(bytes).map_err(|_| "agent_cli_delivery_manifest_invalid")?;
            if manifest.summary.len() > AGENT_CLI_DELIVERY_SUMMARY_MAX_BYTES
                || manifest.summary.contains('\0')
                || manifest.files.len() > AGENT_CLI_DELIVERY_MAX_FILES
                || (manifest.summary.trim().is_empty() && manifest.files.is_empty())
            {
                return Err("agent_cli_delivery_manifest_limit".into());
            }
            let mut seen = BTreeSet::new();
            for file in &manifest.files {
                validate_delivery_path(&file.path)?;
                if !seen.insert(file.path.to_ascii_lowercase()) {
                    return Err("agent_cli_delivery_manifest_limit".into());
                }
            }
            Ok(AgentCliDeliveryDeclaration {
                summary: manifest.summary,
                files: manifest
                    .files
                    .into_iter()
                    .map(|f| AgentCliDeliveryDeclaredFile {
                        path: f.path,
                        expected: None,
                    })
                    .collect(),
            })
        }
        _ => Err("agent_cli_delivery_manifest_version".into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliDeliveryFile {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliDeliveryBundle {
    pub version: u32,
    pub binding_sha256: String,
    pub summary: String,
    pub files: Vec<AgentCliDeliveryFile>,
}
impl AgentCliDeliveryBundle {
    pub fn canonical_bytes(&self, binding: &AgentExecutionBinding) -> Result<Vec<u8>, String> {
        version(self.version)?;
        binding.validate()?;
        if self.binding_sha256 != binding.binding_hash()? {
            return Err("agent_cli_delivery_binding_mismatch".into());
        }
        AgentCliDeliveryManifest {
            version: self.version,
            summary: self.summary.clone(),
            files: self
                .files
                .iter()
                .map(|file| AgentCliDeliveryManifestFile {
                    path: file.path.clone(),
                    sha256: file.sha256.clone(),
                    bytes: file.bytes,
                })
                .collect(),
        }
        .validate()?;
        let mut previous: Option<&str> = None;
        for file in &self.files {
            if previous.is_some_and(|path| path >= file.path.as_str())
                || file.text.contains('\0')
                || file.bytes != file.text.len() as u64
                || file.sha256 != sha256(file.text.as_bytes())
            {
                return Err("agent_cli_delivery_content_mismatch".into());
            }
            previous = Some(&file.path);
        }
        let bytes = serde_json::to_vec(self).map_err(|_| "agent_cli_encoding_failed")?;
        if bytes.is_empty() || bytes.len() > AGENT_CLI_DELIVERY_MAX_BYTES {
            return Err("agent_cli_delivery_bundle_limit".into());
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCliDeliverySnapshot {
    pub version: u32,
    pub binding: AgentExecutionBinding,
    pub artifact_sha256: String,
    pub bundle: AgentCliDeliveryBundle,
}
impl AgentCliDeliverySnapshot {
    pub fn validate_for(&self, binding: &AgentExecutionBinding) -> Result<Vec<u8>, String> {
        version(self.version)?;
        if &self.binding != binding {
            return Err("agent_cli_delivery_binding_mismatch".into());
        }
        let bytes = self.bundle.canonical_bytes(binding)?;
        if self.artifact_sha256 != sha256(&bytes) {
            return Err("agent_cli_delivery_digest_mismatch".into());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
#[path = "agent_cli_tests.rs"]
mod tests;
