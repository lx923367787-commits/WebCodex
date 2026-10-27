//! Bounded CLI protocol completion checks, separate from process exit status.
use crate::cli_adapter::{CliResultContract, CliResultFormat};

const MAX_RESULT_BYTES: usize = 1024 * 1024;
pub struct CliResultVerifier {
    contract: Option<CliResultContract>,
    bytes: Vec<u8>,
    overflow: bool,
}
impl CliResultVerifier {
    pub fn new(contract: Option<CliResultContract>) -> Self {
        Self {
            contract,
            bytes: Vec::new(),
            overflow: false,
        }
    }
    pub fn observe_stdout(&mut self, bytes: &[u8]) {
        if self.contract.is_none() || self.overflow {
            return;
        }
        if self.bytes.len().saturating_add(bytes.len()) > MAX_RESULT_BYTES {
            self.overflow = true;
            self.bytes.clear();
            return;
        }
        self.bytes.extend_from_slice(bytes);
    }
    pub fn finish(&self) -> Result<(), &'static str> {
        let Some(contract) = &self.contract else {
            return Ok(());
        };
        if self.overflow {
            return Err("agent_cli_result_too_large");
        }
        let text = std::str::from_utf8(&self.bytes)
            .map_err(|_| "agent_cli_result_invalid")?
            .trim()
            .trim_start_matches('\u{feff}');
        let values: Vec<serde_json::Value> = match contract.format {
            CliResultFormat::Json => {
                vec![serde_json::from_str(text).map_err(|_| "agent_cli_result_invalid")?]
            }
            CliResultFormat::JsonLines => text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).map_err(|_| "agent_cli_result_invalid"))
                .collect::<Result<_, _>>()?,
        };
        let results: Vec<_> = values
            .iter()
            .filter(
                |value| match (&contract.terminal_field, &contract.terminal_value) {
                    (Some(field), Some(expected)) => {
                        value.get(field).and_then(|v| v.as_str()) == Some(expected.as_str())
                    }
                    (None, None) => true,
                    _ => false,
                },
            )
            .collect();
        if results.len() != 1 {
            return Err("agent_cli_terminal_result_missing_or_ambiguous");
        }
        match results[0]
            .get(&contract.failure_flag)
            .and_then(|value| value.as_bool())
        {
            Some(false) => Ok(()),
            Some(true) => Err("agent_cli_reported_error"),
            None => Err("agent_cli_result_invalid"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contract() -> CliResultContract {
        CliResultContract {
            format: CliResultFormat::Json,
            failure_flag: "is_error".into(),
            terminal_field: None,
            terminal_value: None,
        }
    }
    #[test]
    fn zero_process_exit_cannot_hide_a_cli_error_or_missing_result() {
        let mut verifier = CliResultVerifier::new(Some(contract()));
        verifier.observe_stdout(br#"{"is_error":tr"#);
        verifier.observe_stdout(br#"ue,"subtype":"error_during_execution"}"#);
        assert_eq!(verifier.finish(), Err("agent_cli_reported_error"));
        assert!(CliResultVerifier::new(Some(contract())).finish().is_err());
        let mut success = CliResultVerifier::new(Some(contract()));
        success.observe_stdout(br#"{"is_error":false}"#);
        assert!(success.finish().is_ok());
    }
    #[test]
    fn only_the_declared_terminal_event_counts_and_output_is_bounded() {
        let mut declaration = contract();
        declaration.format = CliResultFormat::JsonLines;
        declaration.terminal_field = Some("type".into());
        declaration.terminal_value = Some("result".into());
        let mut verifier = CliResultVerifier::new(Some(declaration));
        verifier.observe_stdout(
            b"{\"type\":\"tool\",\"is_error\":true}\n{\"type\":\"result\",\"is_error\":false}\n",
        );
        assert!(verifier.finish().is_ok());
        verifier.observe_stdout(b"{\"type\":\"result\",\"is_error\":false}\n");
        assert!(verifier.finish().is_err());
        let mut large = CliResultVerifier::new(Some(contract()));
        large.observe_stdout(&vec![b' '; MAX_RESULT_BYTES + 1]);
        assert_eq!(large.finish(), Err("agent_cli_result_too_large"));
    }
}
