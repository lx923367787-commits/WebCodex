use super::*;
impl WebCodexAdapter {
    pub(crate) async fn workspace_call(
        binary: &Path,
        server: &str,
        token: &Path,
        params: &serde_json::Value,
        cancellation: &CancellationContext,
    ) -> DesktopResult<serde_json::Value> {
        let args = vec![
            "workspaces".into(),
            "--server-url".into(),
            server.into(),
            "--token-file".into(),
            token.to_string_lossy().into_owned(),
            "--json".into(),
        ];
        let input =
            serde_json::to_vec(params).map_err(|_| invalid_contract("workspace request"))?;
        run_json(binary, &args, Some(&input), true, cancellation).await
    }
}
