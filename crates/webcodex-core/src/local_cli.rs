//! Discovery of user-installed command line programs. Discovery is not permission
//! to execute, model availability, or a promise that a login is valid.
use serde::{Deserialize, Serialize};

pub const LOCAL_CLI_DISCOVER_KIND: &str = "local_cli_discover";
pub const LOCAL_CLI_MAX_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalCliQuery {
    pub commands: Vec<String>,
}

impl LocalCliQuery {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=16).contains(&self.commands.len())
            || self.commands.iter().any(|command| {
                command.trim().is_empty()
                    || command.len() > 4096
                    || command.chars().any(char::is_control)
            })
        {
            return Err("local_cli_invalid_commands".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstalledCli {
    pub command: String,
    pub executable: Option<String>,
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalCliInventory {
    pub programs: Vec<InstalledCli>,
    pub execution_mode: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_cli_discovery_accepts_new_products_without_a_provider_enum() {
        LocalCliQuery {
            commands: vec!["new-provider".into(), "custom-model-cli".into()],
        }
        .validate()
        .unwrap();
        assert!(LocalCliQuery { commands: vec![] }.validate().is_err());
        assert!(LocalCliQuery {
            commands: vec!["cli\nother".into()]
        }
        .validate()
        .is_err());
        assert!(LocalCliQuery {
            commands: vec!["cli".into(); 17]
        }
        .validate()
        .is_err());
    }
}
