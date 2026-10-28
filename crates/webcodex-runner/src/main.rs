//! Runner binary composition and startup.
mod webcodex_runner;
use crate::webcodex_runner::cli::parse_args;
use crate::webcodex_runner::cli::RunnerCliAction;
use crate::webcodex_runner::load_config;
use crate::webcodex_runner::registration::process_started_at;
use crate::webcodex_runner::run_runner;
use tracing_subscriber::EnvFilter;
use webcodex_core::{
    apply_edits_shared, apply_patch_shared, artifact_policy, lsp_bridge, mcp_gateway,
    runner_protocol, validation_bridge,
};
use webcodex_runner::{err_cmd, ok_cmd, CommandResult};
use webcodex_runner_config as runner_config;
use webcodex_workspace::{project_overview, workspace_checkpoint};

fn main() {
    if let Some(code) =
        webcodex_runner::detached_job::maybe_run_internal_mode(std::env::args().skip(1))
    {
        std::process::exit(code);
    }
    // Pin the process start timestamp before any transport work so register
    // payloads report real process identity even after reconnect loops.
    let _ = process_started_at();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();

    let action = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    };
    let (config_path, once, stop_on_stdin_eof) = match action {
        RunnerCliAction::ConfigureNativeClis { config_path } => {
            match webcodex_runner::native_cli_setup::configure(&config_path) {
                Ok(value) => {
                    println!("{value}");
                    std::process::exit(0);
                }
                Err(error) => {
                    println!("{}", serde_json::json!({"success":false,"error":error}));
                    std::process::exit(1);
                }
            }
        }
        RunnerCliAction::Run {
            config_path,
            once,
            stop_on_stdin_eof,
        } => (config_path, once, stop_on_stdin_eof),
        RunnerCliAction::Exit {
            code,
            stdout,
            stderr,
        } => {
            if !stdout.is_empty() {
                print!("{}", stdout);
            }
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }
            std::process::exit(code);
        }
    };
    let cfg = match load_config(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    };
    if cfg.token.trim().is_empty() {
        eprintln!(
            "webcodex-runner warning: agent token is empty; connecting without Authorization; the server must be started with --open"
        );
    }
    if let Err(e) = run_runner(cfg, config_path, once, stop_on_stdin_eof) {
        eprintln!("webcodex-runner failed: {}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
use test_support::*;
#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

#[cfg(test)]
use webcodex_runner::jobs::tests as job_manager_tests;
