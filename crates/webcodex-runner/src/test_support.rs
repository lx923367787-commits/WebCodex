#![allow(unused_imports)]
#[cfg(test)]
pub(crate) use crate::webcodex_runner::auto_transport_plan;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::build_ws_request;
pub(crate) use crate::webcodex_runner::cli::parse_args;
pub(crate) use crate::webcodex_runner::cli::parse_runner_args;
pub(crate) use crate::webcodex_runner::cli::usage;
pub(crate) use crate::webcodex_runner::cli::RunnerCliAction;
pub(crate) use crate::webcodex_runner::client_profile_runner_config;
pub(crate) use crate::webcodex_runner::configured_prepared_shell_job_command;
pub(crate) use crate::webcodex_runner::configured_shell_job_command;
pub(crate) use crate::webcodex_runner::configured_validation_job_command;
pub(crate) use crate::webcodex_runner::contains_any;
pub(crate) use crate::webcodex_runner::cwd_allowed;
pub(crate) use crate::webcodex_runner::default_config_path;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::default_quic_alpn;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::default_quic_connect_timeout_secs;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::default_quic_keepalive_interval_secs;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::default_websocket_connect_timeout_secs;
pub(crate) use crate::webcodex_runner::detached_job::handoff_detached_job;
pub(crate) use crate::webcodex_runner::detached_job::snapshot_from_detached_record;
pub(crate) use crate::webcodex_runner::detached_job::DetachedHandoffOutcome;
pub(crate) use crate::webcodex_runner::detached_job::DetachedJobStore;
pub(crate) use crate::webcodex_runner::detached_job::DetachedLaunchSpec;
pub(crate) use crate::webcodex_runner::detached_job::DetachedStartRequest;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::dispatch_request;
pub(crate) use crate::webcodex_runner::dispatch_request_with_outcome;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::effective_transport;
pub(crate) use crate::webcodex_runner::err_cmd;
pub(crate) use crate::webcodex_runner::files::dispatch::handle_file_operation;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::files::dispatch::handle_file_request;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::files::dispatch::is_file_request_kind;
pub(crate) use crate::webcodex_runner::handle_apply_patch_file_request;
pub(crate) use crate::webcodex_runner::handle_apply_text_edits_file_request;
pub(crate) use crate::webcodex_runner::handle_artifact_file_operation;
pub(crate) use crate::webcodex_runner::handle_basic_file_request;
pub(crate) use crate::webcodex_runner::handle_checkpoint_file_request;
pub(crate) use crate::webcodex_runner::handle_write_project_file_request;
pub(crate) use crate::webcodex_runner::hostname;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::is_artifact_request_kind;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::is_basic_file_request_kind;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::is_checkpoint_request_kind;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::is_structured_edit_request_kind;
pub(crate) use crate::webcodex_runner::is_transport_failure;
pub(crate) use crate::webcodex_runner::jobs::context::validate_detached_recovery_context;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::jobs::context::validate_runner_job_context;
pub(crate) use crate::webcodex_runner::jobs::context::validate_runner_job_context_operation;
pub(crate) use crate::webcodex_runner::jobs::context::validate_runner_structured_common;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::decode_failure_prestart_lifecycle;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::job_prestart_lifecycle;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::post_spawn_interruption_delta;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::post_spawn_interruption_lifecycle;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::post_spawn_interruption_reason;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::raw_shell_job_terminal_lifecycle;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::runner_job_is_active;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::runner_job_is_terminal;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::shutdown_target_running;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::JobShutdownBatch;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::JobShutdownOutcome;
pub(crate) use crate::webcodex_runner::jobs::lifecycle::JobShutdownTarget;
pub(crate) use crate::webcodex_runner::jobs::process::cleanup_managed_tree;
pub(crate) use crate::webcodex_runner::jobs::process::join_reader_threads_until;
pub(crate) use crate::webcodex_runner::jobs::process::managed_tree_running;
pub(crate) use crate::webcodex_runner::jobs::process::reap_managed_direct_child;
pub(crate) use crate::webcodex_runner::jobs::process::request_terminate_managed_tree;
pub(crate) use crate::webcodex_runner::jobs::process::spawn_reader;
pub(crate) use crate::webcodex_runner::jobs::process::terminate_managed_tree;
pub(crate) use crate::webcodex_runner::jobs::process::validation_failed_step;
pub(crate) use crate::webcodex_runner::jobs::process::validation_module_available;
pub(crate) use crate::webcodex_runner::jobs::process::wait_failure_error;
pub(crate) use crate::webcodex_runner::jobs::process::wait_managed_tree_exit;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::jobs::test_job_context;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::jobs::test_job_snapshot;
pub(crate) use crate::webcodex_runner::jobs::updates::append_bounded_tail;
pub(crate) use crate::webcodex_runner::jobs::updates::append_runner_stream;
pub(crate) use crate::webcodex_runner::jobs::updates::bounded_runner_error;
pub(crate) use crate::webcodex_runner::jobs::updates::cargo_activity_from_stderr;
pub(crate) use crate::webcodex_runner::jobs::updates::job_update_from_delivery;
pub(crate) use crate::webcodex_runner::jobs::updates::job_update_from_snapshot;
pub(crate) use crate::webcodex_runner::jobs::updates::process_running_activity;
pub(crate) use crate::webcodex_runner::jobs::updates::runner_retained_line_count;
pub(crate) use crate::webcodex_runner::jobs::updates::spawn_job_update_delivery_worker;
pub(crate) use crate::webcodex_runner::jobs::updates::trim_runner_stream_to;
pub(crate) use crate::webcodex_runner::jobs::updates::validation_step_activity;
pub(crate) use crate::webcodex_runner::jobs::updates::JobUpdateDeliveryQueue;
pub(crate) use crate::webcodex_runner::jobs::updates::JobUpdateDeliverySignal;
pub(crate) use crate::webcodex_runner::jobs::updates::JobUpdateDeliverySignalState;
pub(crate) use crate::webcodex_runner::jobs::updates::PendingJobUpdateDelivery;
pub(crate) use crate::webcodex_runner::jobs::updates::JOB_UPDATE_DELIVERY_RETRY;
pub(crate) use crate::webcodex_runner::jobs::updates::JOB_UPDATE_INTERVAL_MS;
pub(crate) use crate::webcodex_runner::jobs::updates::JOB_UPDATE_REQUIRED_PENDING_MAX;
pub(crate) use crate::webcodex_runner::jobs::DetachedJobRef;
pub(crate) use crate::webcodex_runner::jobs::JobManager;
pub(crate) use crate::webcodex_runner::jobs::JobManagerOwnerLifetime;
pub(crate) use crate::webcodex_runner::jobs::OutputChunk;
pub(crate) use crate::webcodex_runner::jobs::PendingJobStart;
pub(crate) use crate::webcodex_runner::jobs::RunnerJobDelta;
pub(crate) use crate::webcodex_runner::jobs::RunningJob;
pub(crate) use crate::webcodex_runner::load_config;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::load_runner_project_summaries_from_dir;
pub(crate) use crate::webcodex_runner::max_concurrent_jobs;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::non_empty_token;
pub(crate) use crate::webcodex_runner::ok_cmd;
pub(crate) use crate::webcodex_runner::output_text::OutputTextDecoder;
pub(crate) use crate::webcodex_runner::output_text::OutputTextSource;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::parse_runner_project_toml;
pub(crate) use crate::webcodex_runner::prepare_detached_process_launch;
pub(crate) use crate::webcodex_runner::project_registry_dir;
pub(crate) use crate::webcodex_runner::projects::creation::write_created_file;
pub(crate) use crate::webcodex_runner::projects::creation::CreatedProjectPaths;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::quic_client_bind_addr_for;
pub(crate) use crate::webcodex_runner::registration::build_register_request;
pub(crate) use crate::webcodex_runner::registration::build_register_request_with_provider_status;
pub(crate) use crate::webcodex_runner::registration::build_shell_profiles_summary;
pub(crate) use crate::webcodex_runner::registration::disable_job_state_reconciliation_for_test;
pub(crate) use crate::webcodex_runner::registration::process_started_at;
pub(crate) use crate::webcodex_runner::registration::register;
pub(crate) use crate::webcodex_runner::registration::register_policy_summary;
pub(crate) use crate::webcodex_runner::registration::runner_build_info;
pub(crate) use crate::webcodex_runner::registration::runner_register_capabilities;
pub(crate) use crate::webcodex_runner::registration::shell_dialect_for_program;
pub(crate) use crate::webcodex_runner::registration::RegisterError;
pub(crate) use crate::webcodex_runner::registration::RegisterErrorKind;
pub(crate) use crate::webcodex_runner::registration::RegisterRecoveryAction;
pub(crate) use crate::webcodex_runner::resolve_prepared_shell_profile;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::resolve_quic_config;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::resolve_quic_server_addrs;
pub(crate) use crate::webcodex_runner::resolve_requested_path;
pub(crate) use crate::webcodex_runner::run_process_with_profiles_and_execution_state_with_start_hook;
pub(crate) use crate::webcodex_runner::run_runner;
pub(crate) use crate::webcodex_runner::run_script_with_profiles_and_execution_state_with_start_hook;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::run_shell;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::runner_project_summary;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::server_url_to_ws;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::sha256_hex_bytes;
pub(crate) use crate::webcodex_runner::shutdown::lock_unpoison;
pub(crate) use crate::webcodex_runner::shutdown::ActivityTracker;
pub(crate) use crate::webcodex_runner::shutdown::BackgroundThreads;
pub(crate) use crate::webcodex_runner::transport::http::bounded_endpoint_path;
pub(crate) use crate::webcodex_runner::transport::http::bounded_response_content_type;
pub(crate) use crate::webcodex_runner::transport::http::bounded_single_line;
pub(crate) use crate::webcodex_runner::transport::http::decode_json_response;
pub(crate) use crate::webcodex_runner::transport::http::error_chain_text;
pub(crate) use crate::webcodex_runner::transport::http::http_status_summary;
pub(crate) use crate::webcodex_runner::transport::http::is_active_instance_lease_conflict;
pub(crate) use crate::webcodex_runner::transport::http::is_unknown_polling_session;
pub(crate) use crate::webcodex_runner::transport::http::looks_like_auth_failure_message;
pub(crate) use crate::webcodex_runner::transport::http::looks_like_fatal_tls_request;
pub(crate) use crate::webcodex_runner::transport::http::looks_like_proxy_html_error;
pub(crate) use crate::webcodex_runner::transport::http::looks_like_server_down_request;
pub(crate) use crate::webcodex_runner::transport::http::looks_like_transient_proxy_response;
pub(crate) use crate::webcodex_runner::transport::http::post_json;
pub(crate) use crate::webcodex_runner::transport::http::post_json_with_auth;
pub(crate) use crate::webcodex_runner::transport::http::read_bounded_response_body;
pub(crate) use crate::webcodex_runner::transport::http::request_error_summary;
pub(crate) use crate::webcodex_runner::transport::http::response_decode_summary;
pub(crate) use crate::webcodex_runner::transport::http::serde_json_category_name;
pub(crate) use crate::webcodex_runner::transport::http::structured_body_error;
pub(crate) use crate::webcodex_runner::transport::http::BoundedResponseBody;
pub(crate) use crate::webcodex_runner::transport::http::RunnerHttpError;
pub(crate) use crate::webcodex_runner::transport::http::RunnerHttpErrorKind;
pub(crate) use crate::webcodex_runner::transport::http::RUNNER_HTTP_RESPONSE_BODY_MAX_BYTES;
pub(crate) use crate::webcodex_runner::transport::http::RUNNER_POLL_PATH;
pub(crate) use crate::webcodex_runner::transport::http::RUNNER_REGISTER_PATH;
pub(crate) use crate::webcodex_runner::transport::polling::handle_one_poll;
pub(crate) use crate::webcodex_runner::transport::polling::PollError;
pub(crate) use crate::webcodex_runner::transport::polling::PollErrorKind;
pub(crate) use crate::webcodex_runner::transport::polling::PollingDispatch;
pub(crate) use crate::webcodex_runner::transport::polling::PollingDispatchCompletion;
pub(crate) use crate::webcodex_runner::transport::polling::PollingDispatchCompletionOnDrop;
pub(crate) use crate::webcodex_runner::transport::polling::PollingDispatchSupervisor;
pub(crate) use crate::webcodex_runner::transport::polling::PollingRecoveryAction;
pub(crate) use crate::webcodex_runner::transport::polling::POLLING_DISPATCH_MAX_IN_FLIGHT;
pub(crate) use crate::webcodex_runner::validate_client_profile;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::validate_project_path_policy;
pub(crate) use crate::webcodex_runner::validate_structured_edit_runner_path;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::websocket_session;
pub(crate) use crate::webcodex_runner::CommandResult;
pub(crate) use crate::webcodex_runner::HotRunnerConfig;
pub(crate) use crate::webcodex_runner::HttpSendConfig;
pub(crate) use crate::webcodex_runner::PreparedShellProfile;
pub(crate) use crate::webcodex_runner::PreparedShellProfileCache;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::QuicClientConfig;
pub(crate) use crate::webcodex_runner::ReloadableRunnerConfig;
pub(crate) use crate::webcodex_runner::RunnerConfig;
pub(crate) use crate::webcodex_runner::RunnerDispatchOutcome;
pub(crate) use crate::webcodex_runner::RunnerPolicy;
pub(crate) use crate::webcodex_runner::RunnerProjectCache;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::RunnerRuntimeState;
pub(crate) use crate::webcodex_runner::RunnerSink;
pub(crate) use crate::webcodex_runner::ShellConfig;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::ShellProfileConfig;
pub(crate) use crate::webcodex_runner::SshConfig;
pub(crate) use crate::webcodex_runner::SshConnectionPool;
pub(crate) use crate::webcodex_runner::SubmitResultError;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::CLIENT_PROFILE_ERROR;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::DEFAULT_MAX_CONCURRENT_JOBS;
#[cfg(test)]
pub(crate) use crate::webcodex_runner::WS_OUTGOING_CAPACITY;
pub(crate) use reqwest::blocking::Client;
#[cfg(test)]
pub(crate) use std::collections::BTreeMap;
pub(crate) use std::collections::HashMap;
pub(crate) use std::collections::HashSet;
pub(crate) use std::collections::VecDeque;
pub(crate) use std::error::Error as StdError;
pub(crate) use std::io::Read;
pub(crate) use std::io::Write;
#[cfg(test)]
pub(crate) use std::net::SocketAddr;
pub(crate) use std::path::Path;
pub(crate) use std::path::PathBuf;
pub(crate) use std::process::Stdio;
pub(crate) use std::sync::atomic::AtomicBool;
pub(crate) use std::sync::atomic::Ordering;
pub(crate) use std::sync::mpsc;
pub(crate) use std::sync::Arc;
pub(crate) use std::sync::Condvar;
pub(crate) use std::sync::Mutex;
pub(crate) use std::sync::Weak;
pub(crate) use std::time::Duration;
pub(crate) use std::time::Instant;
pub(crate) use tracing_subscriber::EnvFilter;
pub(crate) use webcodex_core::apply_edits_shared;
pub(crate) use webcodex_core::apply_patch_shared;
pub(crate) use webcodex_core::artifact_policy;
pub(crate) use webcodex_core::build_info;
pub(crate) use webcodex_core::lsp_bridge;
pub(crate) use webcodex_core::mcp_gateway;
pub(crate) use webcodex_core::runner_job_lifecycle::RunnerJobLifecycle;
pub(crate) use webcodex_core::runner_operation;
pub(crate) use webcodex_core::runner_operation::RunnerFileOperation;
pub(crate) use webcodex_core::runner_operation::RunnerInvocationMetadata;
pub(crate) use webcodex_core::runner_operation::RunnerJobOperation;
#[cfg(test)]
pub(crate) use webcodex_core::runner_operation::RunnerOperation;
pub(crate) use webcodex_core::runner_protocol;
pub(crate) use webcodex_core::runner_protocol::validation_infrastructure_failure_code;
pub(crate) use webcodex_core::runner_protocol::RunnerCapabilities;
#[cfg(test)]
pub(crate) use webcodex_core::runner_protocol::RunnerEnvelope;
pub(crate) use webcodex_core::runner_protocol::RunnerJobUpdateRequest;
pub(crate) use webcodex_core::runner_protocol::RunnerPolicySummary;
pub(crate) use webcodex_core::runner_protocol::RunnerPollPayload;
pub(crate) use webcodex_core::runner_protocol::RunnerPollRequest;
pub(crate) use webcodex_core::runner_protocol::RunnerPollResponse;
pub(crate) use webcodex_core::runner_protocol::RunnerProjectSummary;
pub(crate) use webcodex_core::runner_protocol::RunnerRegisterRequest;
pub(crate) use webcodex_core::runner_protocol::RunnerRegisterResponse;
pub(crate) use webcodex_core::runner_protocol::RunnerRequest;
pub(crate) use webcodex_core::runner_protocol::ShellCommandExecutionState;
pub(crate) use webcodex_core::runner_protocol::ShellJobActivity;
pub(crate) use webcodex_core::runner_protocol::ShellJobActivityPhase;
pub(crate) use webcodex_core::runner_protocol::ShellJobActivitySource;
pub(crate) use webcodex_core::runner_protocol::ShellJobActivityState;
pub(crate) use webcodex_core::runner_protocol::ShellJobContext;
pub(crate) use webcodex_core::runner_protocol::ShellJobInventory;
pub(crate) use webcodex_core::runner_protocol::ShellJobLogSnapshot;
pub(crate) use webcodex_core::runner_protocol::ShellJobSnapshot;
pub(crate) use webcodex_core::runner_protocol::ShellJobStreamSnapshot;
pub(crate) use webcodex_core::runner_protocol::ShellJobValidationProgress;
pub(crate) use webcodex_core::runner_protocol::ShellJobValidationStep;
pub(crate) use webcodex_core::runner_protocol::ShellProfileSummaryEntry;
pub(crate) use webcodex_core::runner_protocol::ShellProfilesSummary;
pub(crate) use webcodex_core::runner_protocol::ShellProjectInventoryPage;
pub(crate) use webcodex_core::runner_protocol::ShellProjectInventoryStatus;
pub(crate) use webcodex_core::runner_protocol::JOB_INVENTORY_MAX_ACTIVE_JOBS;
pub(crate) use webcodex_core::runner_protocol::JOB_INVENTORY_MAX_SERIALIZED_BYTES;
pub(crate) use webcodex_core::runner_protocol::JOB_INVENTORY_MAX_TERMINAL_JOBS;
pub(crate) use webcodex_core::runner_protocol::JOB_SNAPSHOT_STREAM_MAX_BYTES;
pub(crate) use webcodex_core::runner_protocol::JOB_TERMINAL_RETENTION_SECS;
pub(crate) use webcodex_core::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2;
#[cfg(test)]
pub(crate) use webcodex_core::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2_BASELINE_CAPABILITY_NAMES;
pub(crate) use webcodex_core::runner_protocol::VALIDATION_STEP_SPAWN_FAILED_CODE;
pub(crate) use webcodex_core::runner_protocol::VALIDATION_STEP_WAIT_FAILED_CODE;
pub(crate) use webcodex_core::runner_protocol::VALIDATION_TOOL_UNAVAILABLE_CODE;
pub(crate) use webcodex_core::validation_bridge;
pub(crate) use webcodex_process::GracefulTermination;
pub(crate) use webcodex_process::ManagedChild;
pub(crate) use webcodex_runner_config as runner_config;
#[cfg(test)]
pub(crate) use webcodex_runner_config::TRANSPORT_AUTO;
#[cfg(test)]
pub(crate) use webcodex_runner_config::TRANSPORT_POLLING;
#[cfg(test)]
pub(crate) use webcodex_runner_config::TRANSPORT_QUIC;
#[cfg(test)]
pub(crate) use webcodex_runner_config::TRANSPORT_WEBSOCKET;
pub(crate) use webcodex_workspace::project_overview;
pub(crate) use webcodex_workspace::workspace_checkpoint;
