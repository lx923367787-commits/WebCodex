//! Runner command-line parsing.

use crate::webcodex_runner::{
    client_profile_runner_config, default_config_path, validate_client_profile,
};
use std::path::PathBuf;
use webcodex_core::build_info;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerCliAction {
    Run {
        config_path: PathBuf,
        once: bool,
        stop_on_stdin_eof: bool,
    },
    Exit {
        code: i32,
        stdout: String,
        stderr: String,
    },
}

pub(crate) fn usage() -> &'static str {
    "Usage: webcodex-runner [--config PATH] [--once] [--stop-on-stdin-eof]\n\n\
     Options:\n\
       -h, --help                 Print help and exit\n\
       -V, --version              Print version and exit\n\
       -c, --config PATH          Runner config path for normal runtime\n\
       --profile NAME             Client config profile for default config path\n\
       --once                     Complete one successful poll, then exit (polling transport)\n\
       --stop-on-stdin-eof        Stop when the invoking parent closes stdin\n\n\
     With --profile, the default config path is derived under\n\
     /etc/webcodex/clients/<profile> for root or\n\
     ~/.config/webcodex/clients/<profile> for non-root users. Explicit\n\
     --config overrides the profile-derived default.\n\n\
     Environment:\n\
       WEBCODEX_RUNNER_CONFIG     default config path override\n\
       WEBCODEX_AGENT_CONFIG      legacy alias for WEBCODEX_RUNNER_CONFIG\n\
     Example runner.toml:\n\
       server_url = \"https://v4.yyjeqhc.cn\"\n\
       token = \"...\"\n\
       client_id = \"xrh\"\n\
       display_name = \"XRH\"\n\
       owner = \"yyjeqhc\"\n\
       project_registry_dir = \"/root/.config/webcodex/project-registry\"\n\
       poll_interval_ms = 1000\n\
\n\
       [policy]\n\
       allow_raw_shell = true\n\
       allow_cwd_anywhere = true\n\
       max_timeout_secs = 3600\n\
       max_output_bytes = 262144\n"
}

pub(crate) fn parse_args() -> Result<RunnerCliAction, String> {
    parse_runner_args(std::env::args().skip(1))
}

pub(crate) fn parse_runner_args<I, S>(args: I) -> Result<RunnerCliAction, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect();
    if args.len() == 1 {
        match args[0].as_str() {
            "--help" | "-h" => {
                return Ok(RunnerCliAction::Exit {
                    code: 0,
                    stdout: usage().to_string(),
                    stderr: String::new(),
                });
            }
            "--version" | "-V" => {
                return Ok(RunnerCliAction::Exit {
                    code: 0,
                    stdout: build_info::version_output("webcodex-runner"),
                    stderr: String::new(),
                });
            }
            _ => {}
        }
    }
    let runner_config_env = std::env::var("WEBCODEX_RUNNER_CONFIG").ok();
    let legacy_agent_config_env = std::env::var("WEBCODEX_AGENT_CONFIG").ok();
    let mut config_path: Option<PathBuf> = None;
    let mut profile: Option<String> = None;
    let mut once = false;
    let mut stop_on_stdin_eof = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                return Ok(RunnerCliAction::Exit {
                    code: 0,
                    stdout: usage().to_string(),
                    stderr: String::new(),
                });
            }
            "--version" | "-V" => {
                return Ok(RunnerCliAction::Exit {
                    code: 0,
                    stdout: build_info::version_output("webcodex-runner"),
                    stderr: String::new(),
                });
            }
            "--once" => once = true,
            "--stop-on-stdin-eof" => stop_on_stdin_eof = true,
            "--config" | "-c" => {
                let Some(path) = args.next() else {
                    return Err("--config requires a path".to_string());
                };
                config_path = Some(PathBuf::from(path));
            }
            "--profile" => {
                let Some(value) = args.next() else {
                    return Err("--profile requires a value".to_string());
                };
                profile = Some(value);
            }
            _ => return Err(format!("unknown argument: {}\n{}", arg, usage())),
        }
    }
    let profile = profile
        .as_deref()
        .map(validate_client_profile)
        .transpose()?;
    let config_path = if let Some(config_path) = config_path {
        config_path
    } else {
        if let Some(profile) = profile {
            client_profile_runner_config(&profile)?
        } else {
            if runner_config_env.is_some() && legacy_agent_config_env.is_some() {
                return Err(
                    "WEBCODEX_RUNNER_CONFIG and legacy WEBCODEX_AGENT_CONFIG cannot both be set"
                        .to_string(),
                );
            }
            runner_config_env
                .or(legacy_agent_config_env)
                .map(PathBuf::from)
                .map(Ok)
                .unwrap_or_else(default_config_path)?
        }
    };
    Ok(RunnerCliAction::Run {
        config_path,
        once,
        stop_on_stdin_eof,
    })
}
