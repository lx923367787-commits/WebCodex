//! Runner jobs context implementation.

use std::collections::HashSet;
#[cfg(test)]
use webcodex_core::runner_operation;
use webcodex_core::runner_operation::RunnerJobOperation;
use webcodex_core::runner_protocol;
#[cfg(test)]
use webcodex_core::runner_protocol::RunnerRequest;
use webcodex_core::runner_protocol::ShellJobContext;

pub(crate) fn validate_detached_recovery_context(
    context: &ShellJobContext,
    client_id: &str,
) -> Result<(), String> {
    const MAX_CONTEXT_FIELD_CHARS: usize = 1_024;
    const MAX_COMMAND_PREVIEW_CHARS: usize = 121;
    let bounded =
        |value: &str, max_chars: usize| !value.contains('\0') && value.chars().count() <= max_chars;
    if !bounded(&context.command_preview, MAX_COMMAND_PREVIEW_CHARS)
        || context.command_preview.contains(['\r', '\n'])
    {
        return Err("detached Job recovery command_preview is invalid or oversized".to_string());
    }
    for (name, value) in [
        ("ssh_resource", context.ssh_resource.as_deref()),
        ("project_cwd", context.project_cwd.as_deref()),
        ("cwd", context.cwd.as_deref()),
        ("purpose", context.purpose.as_deref()),
        ("shell", context.shell.as_deref()),
    ] {
        if value.is_some_and(|value| !bounded(value, MAX_CONTEXT_FIELD_CHARS)) {
            return Err(format!(
                "detached Job recovery context {name} is invalid or oversized"
            ));
        }
    }
    if context.ssh_resource.is_some() && context.workflow_session_id.is_none() {
        return Err("detached Job recovery SSH resource requires a Workflow Session".to_string());
    }
    if context.purpose.as_deref().is_some_and(|purpose| {
        !matches!(
            purpose,
            "validation"
                | "test"
                | "build"
                | "format"
                | "release"
                | "diagnostic"
                | "operation"
                | "other"
        )
    }) {
        return Err("detached Job recovery purpose is invalid".to_string());
    }
    if context.shell.as_deref().is_some_and(|shell| {
        !matches!(
            shell,
            "sh" | "bash" | "powershell" | "configured" | "custom" | "remote" | "direct_argv"
        )
    }) {
        return Err("detached Job recovery shell is invalid".to_string());
    }
    if !context.validation_steps.is_empty() {
        if !(1..=3).contains(&context.validation_steps.len())
            || context
                .validation_steps
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != context.validation_steps.len()
            || context
                .validation_steps
                .iter()
                .any(|step| !matches!(step.as_str(), "format" | "check" | "test"))
        {
            return Err("detached Job recovery validation_steps are invalid".to_string());
        }
    }
    if context.validation.as_ref().is_some_and(|metadata| {
        !metadata.is_valid()
            || metadata
                .steps
                .iter()
                .map(|step| step.name.clone())
                .collect::<Vec<_>>()
                != context.validation_steps
    }) {
        return Err("detached Job recovery validation metadata is invalid".to_string());
    }
    if context
        .structured_execution
        .as_ref()
        .is_some_and(|metadata| !metadata.is_valid())
    {
        return Err("detached Job recovery structured execution metadata is invalid".to_string());
    }
    if let Some(project_id) = context.runtime_project_id.as_deref() {
        let prefix = format!("agent:{client_id}:");
        if !bounded(project_id, MAX_CONTEXT_FIELD_CHARS)
            || project_id
                .strip_prefix(&prefix)
                .is_none_or(|suffix| suffix.is_empty())
        {
            return Err("detached Job recovery project does not match the runner".to_string());
        }
    }
    if let Some(session_id) = context.workflow_session_id.as_deref() {
        if context.runtime_project_id.is_none()
            || session_id.len() > 128
            || !session_id.starts_with("wc_sess_")
            || !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("detached Job recovery Workflow Session is invalid".to_string());
        }
    }
    Ok(())
}

pub(crate) fn validate_runner_job_context_operation(
    context: &ShellJobContext,
    operation: &RunnerJobOperation,
    client_id: &str,
) -> Result<(), String> {
    const MAX_CONTEXT_FIELD_CHARS: usize = 1_024;
    const MAX_COMMAND_PREVIEW_CHARS: usize = 121;
    let bounded =
        |value: &str, max_chars: usize| !value.contains('\0') && value.chars().count() <= max_chars;
    if !operation.is_start() {
        return Err("stop_job is not a Job start operation".to_string());
    }
    if operation.context() != Some(context) {
        return Err("job recovery context does not match the typed Job operation".to_string());
    }
    if !bounded(&context.command_preview, MAX_COMMAND_PREVIEW_CHARS)
        || context.command_preview.contains(['\r', '\n'])
    {
        return Err("job recovery context command_preview is invalid or oversized".to_string());
    }
    for (name, value) in [
        ("ssh_resource", context.ssh_resource.as_deref()),
        ("project_cwd", context.project_cwd.as_deref()),
        ("cwd", context.cwd.as_deref()),
        ("purpose", context.purpose.as_deref()),
        ("shell", context.shell.as_deref()),
    ] {
        if value.is_some_and(|value| !bounded(value, MAX_CONTEXT_FIELD_CHARS)) {
            return Err(format!(
                "job recovery context {name} is invalid or oversized"
            ));
        }
    }
    if context.cwd.as_deref() != operation.cwd() {
        return Err("job recovery context cwd does not match the execution request".to_string());
    }
    if context.ssh_resource.is_some() && context.workflow_session_id.is_none() {
        return Err("job recovery context SSH resource requires a Workflow Session".to_string());
    }
    if context.purpose.as_deref().is_some_and(|purpose| {
        !matches!(
            purpose,
            "validation"
                | "test"
                | "build"
                | "format"
                | "release"
                | "diagnostic"
                | "operation"
                | "other"
        )
    }) {
        return Err("job recovery context purpose is invalid".to_string());
    }
    if context.shell.as_deref().is_some_and(|shell| {
        !matches!(
            shell,
            "sh" | "bash" | "powershell" | "configured" | "custom" | "remote" | "direct_argv"
        )
    }) {
        return Err("job recovery context shell is invalid".to_string());
    }

    let validation_steps = match operation {
        RunnerJobOperation::StartValidation(operation) => {
            let names = operation
                .steps
                .iter()
                .map(|step| step.name.clone())
                .collect::<Vec<_>>();
            if !(1..=3).contains(&operation.steps.len())
                || operation.steps.iter().any(|step| !step.is_canonical())
                || operation.steps.iter().enumerate().any(|(index, step)| {
                    operation.steps[..index]
                        .iter()
                        .any(|earlier| earlier.name == step.name)
                })
            {
                return Err("invalid structured validation plan".to_string());
            }
            names
        }
        _ => Vec::new(),
    };
    if context.validation_steps != validation_steps
        || context
            .validation_steps
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != context.validation_steps.len()
        || context
            .validation_steps
            .iter()
            .any(|step| !matches!(step.as_str(), "format" | "check" | "test"))
    {
        return Err("job recovery context validation_steps are invalid".to_string());
    }
    let validation_context = matches!(operation, RunnerJobOperation::StartValidation(_));
    if context.validation.as_ref().is_some_and(|metadata| {
        !validation_context
            || !metadata.is_valid()
            || metadata
                .steps
                .iter()
                .map(|step| step.name.clone())
                .collect::<Vec<_>>()
                != context.validation_steps
    }) {
        return Err("job recovery context validation metadata is invalid".to_string());
    }
    if context
        .structured_execution
        .as_ref()
        .is_some_and(|metadata| !metadata.is_valid())
    {
        return Err("job recovery context structured execution metadata is invalid".to_string());
    }

    match operation {
        RunnerJobOperation::StartShell(operation) => {
            runner_protocol::validate_raw_shell_wire_command(&operation.command)?;
        }
        RunnerJobOperation::StartProcess(operation)
        | RunnerJobOperation::StartDetachedProcess(operation) => {
            if context.ssh_resource.is_some() {
                return Err("typed process Job request shape is invalid".to_string());
            }
            runner_protocol::validate_process_argv(&operation.process)?;
            validate_runner_structured_common(
                operation.cwd.as_deref(),
                operation.stdin.as_deref(),
                operation.timeout_secs,
            )?;
        }
        RunnerJobOperation::StartScript(operation) => {
            if context.ssh_resource.is_some() {
                return Err("typed script Job request shape is invalid".to_string());
            }
            runner_protocol::validate_script_request(
                &operation.script,
                operation.stdin.as_deref(),
                operation.cwd.as_deref(),
                operation.timeout_secs,
            )?;
        }
        RunnerJobOperation::StartValidation(_) => {}
        RunnerJobOperation::Stop { .. } => unreachable!("stop rejected above"),
    }

    if context.structured_execution != operation.expected_structured_execution() {
        return Err(
            "job recovery context structured execution metadata does not match request".to_string(),
        );
    }
    if let Some(project_id) = context.runtime_project_id.as_deref() {
        let prefix = format!("agent:{client_id}:");
        if !bounded(project_id, MAX_CONTEXT_FIELD_CHARS)
            || project_id
                .strip_prefix(&prefix)
                .is_none_or(|suffix| suffix.is_empty())
        {
            return Err(
                "job recovery context runtime_project_id does not match the runner".to_string(),
            );
        }
    }
    if let Some(session_id) = context.workflow_session_id.as_deref() {
        if context.runtime_project_id.is_none()
            || session_id.len() > 128
            || !session_id.starts_with("wc_sess_")
            || !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("job recovery context workflow_session_id is invalid".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn validate_runner_job_context(
    context: &ShellJobContext,
    request: &RunnerRequest,
    client_id: &str,
) -> Result<(), String> {
    let mut request = request.clone();
    request.job_context = Some(context.clone());
    match request.decode_operation()? {
        runner_operation::RunnerOperation::Job(operation) => {
            validate_runner_job_context_operation(context, &operation, client_id)
        }
        _ => Err("request is not a Runner Job operation".to_string()),
    }
}

pub(crate) fn validate_runner_structured_common(
    cwd: Option<&str>,
    stdin: Option<&str>,
    timeout_secs: u64,
) -> Result<(), String> {
    if let Some(stdin) = stdin {
        if stdin.len() > runner_protocol::PROCESS_STDIN_MAX_BYTES {
            return Err(format!(
                "stdin is too large; maximum is {} bytes",
                runner_protocol::PROCESS_STDIN_MAX_BYTES
            ));
        }
        if stdin.contains('\0') {
            return Err("stdin cannot contain NUL bytes".to_string());
        }
    }
    if let Some(cwd) = cwd {
        if cwd.len() > runner_protocol::PROCESS_CWD_MAX_BYTES {
            return Err(format!(
                "cwd is too long; maximum is {} bytes",
                runner_protocol::PROCESS_CWD_MAX_BYTES
            ));
        }
        if cwd.contains('\0') {
            return Err("cwd cannot contain NUL bytes".to_string());
        }
    }
    if !(runner_protocol::STRUCTURED_EXECUTION_TIMEOUT_MIN_SECS
        ..=runner_protocol::STRUCTURED_EXECUTION_TIMEOUT_MAX_SECS)
        .contains(&timeout_secs)
    {
        return Err(format!(
            "timeout_secs must be between {} and {}",
            runner_protocol::STRUCTURED_EXECUTION_TIMEOUT_MIN_SECS,
            runner_protocol::STRUCTURED_EXECUTION_TIMEOUT_MAX_SECS
        ));
    }
    Ok(())
}
