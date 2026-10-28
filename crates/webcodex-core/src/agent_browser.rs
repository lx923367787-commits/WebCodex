//! Fixed local-browser actions bound to an existing ordinary program.
//! No caller-selected URL, executable, profile, script, environment or path.
use crate::agent_program::AgentProgramBinding;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const AGENT_BROWSER_KIND: &str = "agent_local_browser";
pub const MAX_BROWSER_RESULT_BYTES: usize = 3 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserStep {
    Navigate {
        path: String,
    },
    Click {
        selector: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        accept_dialog: bool,
    },
    Fill {
        selector: String,
        text: String,
    },
    Select {
        selector: String,
        value: String,
    },
    Text {
        selector: String,
    },
    ExpectText {
        selector: String,
        text: String,
    },
    Screenshot {},
}
pub fn validate_steps(steps: &[BrowserStep]) -> Result<(), String> {
    if steps.is_empty() || steps.len() > 40 {
        return Err("agent_browser_step_limit".into());
    }
    for step in steps {
        match step {
            BrowserStep::Navigate { path } => {
                if !path.starts_with('/')
                    || path.starts_with("//")
                    || path.contains('\\')
                    || path.len() > 4096
                    || path.chars().any(char::is_control)
                {
                    return Err("agent_browser_relative_path_required".into());
                }
            }
            BrowserStep::Screenshot {} => {}
            BrowserStep::Click { selector, .. }
            | BrowserStep::Text { selector }
            | BrowserStep::ExpectText { selector, .. }
            | BrowserStep::Fill { selector, .. }
            | BrowserStep::Select { selector, .. } => {
                if selector.trim().is_empty()
                    || selector.len() > 1000
                    || selector.contains(">>")
                    || selector.contains('\0')
                {
                    return Err("agent_browser_css_selector_invalid".into());
                }
            }
        }
        let value = match step {
            BrowserStep::Fill { text, .. } => Some(text),
            BrowserStep::ExpectText { text, .. } => {
                if text.trim().is_empty() {
                    return Err("agent_browser_expected_text_required".into());
                }
                Some(text)
            }
            BrowserStep::Select { value, .. } => Some(value),
            _ => None,
        };
        if value.is_some_and(|s| s.len() > 16384 || s.contains('\0')) {
            return Err("agent_browser_value_limit".into());
        }
    }
    if serde_json::to_vec(steps)
        .map_err(|_| "agent_browser_encoding_failed")?
        .len()
        > 48 * 1024
    {
        return Err("agent_browser_step_limit".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBrowserAction {
    pub action_id: String,
    pub service: AgentProgramBinding,
    pub steps: Vec<BrowserStep>,
    pub timeout_secs: u64,
}
impl AgentBrowserAction {
    pub fn validate(&self) -> Result<(), String> {
        self.service.validate()?;
        validate_steps(&self.steps)?;
        if serde_json::to_vec(self)
            .map_err(|_| "agent_browser_encoding_failed")?
            .len()
            > 60000
        {
            return Err("agent_browser_payload_limit".into());
        }
        if !self.action_id.strip_prefix("wc_browser_").is_some_and(|s| {
            s.len() == 32
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        }) || self.service.local_service_port.is_none()
            || !(1..=30).contains(&self.timeout_secs)
        {
            return Err("agent_browser_binding_invalid".into());
        }
        Ok(())
    }
    pub fn identity_hash(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|_| "agent_browser_encoding_failed")?)
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBrowserRequest {
    pub target: crate::runner_protocol::AgentExecutionTarget,
    pub action: AgentBrowserAction,
    pub observe_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_step: Option<u16>,
}
impl AgentBrowserRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.action.validate()?;
        if self.image_step.is_some_and(|i| {
            !self.observe_only
                || !matches!(
                    self.action.steps.get(i as usize),
                    Some(BrowserStep::Screenshot {})
                )
        }) {
            return Err("agent_browser_image_step_invalid".into());
        }
        self.target.validate()?;
        if self.target.client_id != self.action.service.target.client_id
            || (!self.observe_only && self.target != self.action.service.target)
        {
            return Err("agent_browser_target_mismatch".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBrowserReceipt {
    pub action_id: String,
    pub identity_hash: String,
    pub state: String,
    pub process_id: Option<u32>,
    pub exit_code: Option<i32>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}
impl AgentBrowserReceipt {
    pub fn validate_for(&self, action: &AgentBrowserAction) -> Result<(), String> {
        if self.action_id != action.action_id
            || self.identity_hash != action.identity_hash()?
            || !matches!(
                self.state.as_str(),
                "dispatch_fenced" | "completed" | "failed" | "not_started" | "outcome_unknown"
            )
            || self.process_id == Some(0)
            || (self.state == "completed"
                && (self.exit_code != Some(0)
                    || self.process_id.is_none()
                    || self.result.as_ref().is_none_or(|v| v["success"] != true)))
            || (self.state == "failed" && (self.exit_code.is_none() || self.process_id.is_none()))
            || (self.state == "not_started"
                && (self.exit_code.is_some() || self.process_id.is_some()))
            || self.error.as_ref().is_some_and(|e| {
                e.len() > 128 || !e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            })
            || serde_json::to_vec(self)
                .map_err(|_| "agent_browser_encoding_failed")?
                .len()
                > MAX_BROWSER_RESULT_BYTES
        {
            return Err("agent_browser_receipt_invalid".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "agent_browser_tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBrowserImage {
    pub step: u16,
    pub content_base64: String,
    pub file_bytes: usize,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBrowserResponse {
    pub receipt: AgentBrowserReceipt,
    pub image: Option<AgentBrowserImage>,
}
impl AgentBrowserResponse {
    pub fn validate_for(&self, query: &AgentBrowserRequest) -> Result<(), String> {
        self.receipt.validate_for(&query.action)?;
        if let Some(image) = &self.image {
            if Some(image.step) != query.image_step
                || !matches!(self.receipt.state.as_str(), "completed" | "failed")
                || image.file_bytes == 0
                || image.file_bytes > 1024 * 1024
                || image.content_base64.len() > 1400000
                || image.sha256.len() != 64
                || !image
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err("agent_browser_image_response_invalid".into());
            }
        } else if query.image_step.is_some() {
            return Err("agent_browser_image_missing".into());
        }
        Ok(())
    }
}
