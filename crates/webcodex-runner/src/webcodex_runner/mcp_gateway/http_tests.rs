use super::*;
use std::net::TcpListener;
use std::thread;

fn server(
    replies: Vec<(u16, &'static str, String, bool)>,
) -> (String, thread::JoinHandle<Vec<(String, Value)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut requests = Vec::new();
        for (status, content_type, body, session) in replies {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(stream) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                assert!(!line.is_empty());
                headers.push_str(&line);
            }
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            requests.push((headers, serde_json::from_slice(&bytes).unwrap()));
            let session = if session {
                "Mcp-Session-Id: fixture-session\r\n"
            } else {
                ""
            };
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{session}\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    (url, handle)
}

fn config(url: String) -> McpGatewayProviderConfig {
    McpGatewayProviderConfig {
        id: "http-test".into(),
        name: "HTTP test".into(),
        executable: String::new(),
        url: Some(url),
        headers: BTreeMap::from([("Authorization".into(), "Bearer fixture-only".into())]),
        env: BTreeMap::new(),
        args: vec![],
        cwd: None,
        env_from_env: BTreeMap::new(),
        timeout_secs: None,
    }
}

fn initialized() -> String {
    json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}).to_string()
}

#[test]
fn http_session_pagination_and_streamed_call_work_without_replay() {
    let (url, fixture) = server(vec![
        (200, "application/json", initialized(), true),
        (202, "application/json", String::new(), false),
        (200, "application/json", json!({"jsonrpc":"2.0","id":2,"result":{"tools":[],"nextCursor":"second"}}).to_string(), false),
        (200, "application/json", json!({"jsonrpc":"2.0","id":3,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}).to_string(), false),
        (200, "text/event-stream", "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n\n".into(), false),
    ]);
    let mut connection = ProviderConnection::spawn(&config(url), Duration::from_secs(3))
        .ok()
        .unwrap();
    assert_eq!(
        connection
            .tools_list(Duration::from_secs(3))
            .ok()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        connection
            .tools_call("echo", json!({}), Duration::from_secs(3))
            .ok()
            .unwrap()
            .content,
        vec![McpGatewayContent::Text { text: "ok".into() }]
    );
    let requests = fixture.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(!requests[0].0.to_lowercase().contains("mcp-session-id:"));
    for (headers, _) in &requests[1..] {
        assert!(headers
            .to_lowercase()
            .contains("mcp-session-id: fixture-session"));
    }
    assert_eq!(requests[3].1["params"]["cursor"], "second");
    assert!(requests.iter().all(|(headers, _)| headers
        .to_lowercase()
        .contains("authorization: bearer fixture-only")));
}

#[test]
fn http_auth_failure_does_not_publish_or_expose_response_body() {
    let (url, fixture) = server(vec![(
        401,
        "text/plain",
        "secret-fixture-error".into(),
        false,
    )]);
    let manager = McpGatewayManager::new(&McpGatewayConfig::default());
    let error = manager.prepare(&config(url)).err().unwrap();
    assert_eq!(error, "provider_auth_required");
    assert!(manager.provider_inventory().is_empty());
    assert_eq!(fixture.join().unwrap().len(), 1);
}

#[test]
fn sse_responses_enforce_correlation_and_bounds() {
    let error = read_sse_response(
        "data: {\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{}}\n\n".as_bytes(),
        1,
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "provider_unknown_response_id");
    let oversized = format!("data: {}\n\n", "x".repeat(MCP_GATEWAY_MAX_MESSAGE_BYTES));
    assert_eq!(
        read_sse_response(oversized.as_bytes(), 1)
            .err()
            .unwrap()
            .code,
        "provider_message_too_large"
    );
}

#[test]
fn expired_http_session_is_not_replayed_and_next_request_reinitializes() {
    let list = |id| json!({"jsonrpc":"2.0", "id":id, "result":{"tools":[]}}).to_string();
    let (url, fixture) = server(vec![
        (200, "application/json", initialized(), true),
        (202, "application/json", String::new(), false),
        (200, "application/json", list(2), false),
        (404, "application/json", "{}".into(), false),
        (200, "application/json", initialized(), true),
        (202, "application/json", String::new(), false),
        (200, "application/json", list(2), false),
    ]);
    let manager = McpGatewayManager::new(&McpGatewayConfig::default());
    let prepared = manager.prepare(&config(url)).unwrap();
    let inventory = manager.commit(prepared);
    let request = || McpGatewayRequest::ToolsList {
        provider_id: inventory.provider_id.clone(),
        provider_instance_id: inventory.provider_instance_id.clone(),
    };
    let failed = manager.handle(request());
    assert_eq!(
        failed.error.unwrap().code,
        "provider_http_session_unavailable"
    );
    assert_eq!(manager.connection_status()[0].state, "error");
    assert_eq!(
        manager.last_error("http-test"),
        Some("provider_http_session_unavailable")
    );
    assert!(manager.handle(request()).error.is_none());
    assert_eq!(manager.connection_status()[0].state, "connected");
    let requests = fixture.join().unwrap();
    assert_eq!(requests.len(), 7);
    assert_eq!(
        requests
            .iter()
            .filter(|(_, value)| value["method"] == "tools/list")
            .count(),
        3
    );
}
