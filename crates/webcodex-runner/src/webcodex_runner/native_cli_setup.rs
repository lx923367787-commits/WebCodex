//! Compatibility entry: CLI registration is automatic on Runner load.
use std::path::Path;
pub(crate) fn configure(path: &Path) -> Result<serde_json::Value, String> {
    let config = super::config::load_config(path)?;
    Ok(serde_json::json!({"success":true,"restartRequired":true,
        "resourceIds":config.agent_execution_resources.profiles.iter().map(|p| &p.id).collect::<Vec<_>>(),
        "cliLoadErrors":config.agent_execution_resources.native_cli.as_ref().map(|c| &c.load_errors),
        "message":"Runner 使用同一本机 CLI 目录自动加载执行资源；配置检查通过不代表已加载或执行成功。"}))
}
