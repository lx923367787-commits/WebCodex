use super::*;

#[test]
fn workspace_bridge_accepts_only_exact_loopback_arguments_and_bounded_json() {
    let args = |server: &str| {
        vec![
            "--server-url".into(),
            server.into(),
            "--token-file".into(),
            "private-token".into(),
            "--json".into(),
        ]
    };
    assert!(parse(&args("http://127.0.0.1:51140")).is_ok());
    for server in [
        "https://remote.example",
        "http://localhost:51140",
        "http://127.0.0.1:51140/other",
        "http://u:p@127.0.0.1:51140",
        "http://127.0.0.1:51140/?a=b",
    ] {
        assert!(parse(&args(server)).is_err());
    }
    let mut extra = args("http://127.0.0.1:51140");
    extra.push("--tool".into());
    assert!(parse(&extra).is_err());
    assert!(
        read_input(&b"{\"action\":\"native_workspaces_read\",\"host_id\":\"host\"}"[..]).is_ok()
    );
    assert!(read_input(&vec![b' '; 131073][..]).is_err());
}
