use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn options(config: PathBuf, token_file: PathBuf, url: String, action: &str) -> Options {
    Options {
        action: action.into(),
        config,
        server_url: url,
        client_id: "runner".into(),
        token_file,
        project: (action != "list").then(|| "agent:runner:repo".into()),
        expected_revision: (action != "list").then(|| format!("sha256:{}", "a".repeat(64))),
        idempotency_key: (action != "list").then(|| "same-request".into()),
    }
}

fn server(status: u16, response: Value) -> (String, std::thread::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("fixture accept: {error}"),
            }
        };
        // Accepted sockets inherit the listener's nonblocking mode on Windows.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let (head, body) = loop {
            let mut chunk = [0; 4096];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
            assert!(bytes.len() < 16384);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break (
                        head,
                        serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length]).unwrap(),
                    );
                }
            }
        };
        assert!(head
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-credential"));
        let response = response.to_string();
        write!(stream, "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        (head.lines().next().unwrap().to_owned(), body)
    });
    (url, worker)
}

fn files(temp: &tempfile::TempDir, url: &str) -> (PathBuf, PathBuf) {
    let config = temp.path().join("runner.toml");
    let token = temp.path().join("credential");
    std::fs::write(
        &config,
        format!("server_url={url:?}\nclient_id=\"runner\"\ntoken=\"private-runner-value\"\n"),
    )
    .unwrap();
    std::fs::write(&token, "fixture-credential").unwrap();
    (config, token)
}

#[tokio::test]
async fn project_management_lists_enabled_and_disabled_on_exact_runner() {
    let (url, thread) = server(
        200,
        json!({"success":true,"output":{"projects":[
            {"id":"agent:runner:repo","name":"Repo","path":"C:/work/repo","revision":"sha256:one","enabled":false,"connected":true,"agent_status":"online","active_jobs":0}
        ]}}),
    );
    let temp = tempfile::tempdir().unwrap();
    let (config, token) = files(&temp, &url);
    let output: Value =
        serde_json::from_str(&run(options(config, token, url, "list")).await.unwrap()).unwrap();
    assert_eq!(output["projects"][0]["enabled"], false);
    assert_eq!(output["client_id"], "runner");
    assert!(!output.to_string().contains("private-runner-value"));
    let (path, body) = thread.join().unwrap();
    assert_eq!(path, "POST /api/projects/list HTTP/1.1");
    assert_eq!(body, json!({"client_id":"runner"}));
}

fn inventory_project(index: usize) -> Value {
    json!({
        "id":format!("agent:runner:repo-{index}"),
        "name":format!("Repo {index}"),
        "path":format!("C:/work/repo-{index}"),
        "revision":format!("sha256:revision-{index}"),
        "enabled":index % 2 == 0,
        "connected":true,
        "agent_status":"online",
        "active_jobs":0
    })
}

#[tokio::test]
async fn project_management_preserves_complete_inventory_metadata_without_other_output_fields() {
    let projects = vec![inventory_project(0)];
    let (url, thread) = server(
        200,
        json!({
            "success":true,
            "output":{"projects":projects,"count":1,"matched_count":1,"truncated":false,"operator_user_id":"actual-user-id",
                "token":"private-response-value","recommended_for_smoke":["internal-reference"]}
        }),
    );
    let temp = tempfile::tempdir().unwrap();
    let (config, token) = files(&temp, &url);
    let output: Value =
        serde_json::from_str(&run(options(config, token, url, "list")).await.unwrap()).unwrap();
    assert_eq!(output["projects"], json!(projects));
    assert_eq!(output["matched_count"], 1);
    assert_eq!(output["truncated"], false);
    assert_eq!(output["operator_user_id"], "actual-user-id");
    let mut keys: Vec<_> = output
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "client_id",
            "matched_count",
            "operator_user_id",
            "projects",
            "truncated"
        ]
    );
    assert!(!output.to_string().contains("private-response-value"));
    assert!(!output.to_string().contains("fixture-credential"));
    let (path, body) = thread.join().unwrap();
    assert_eq!(path, "POST /api/projects/list HTTP/1.1");
    assert_eq!(body, json!({"client_id":"runner"}));
}

#[tokio::test]
async fn project_management_preserves_truncated_inventory_metadata_and_returned_page() {
    let projects: Vec<_> = (0..100).map(inventory_project).collect();
    let (url, thread) = server(
        200,
        json!({
            "success":true,"output":{"projects":projects,"matched_count":137,"truncated":true}
        }),
    );
    let temp = tempfile::tempdir().unwrap();
    let (config, token) = files(&temp, &url);
    let output: Value =
        serde_json::from_str(&run(options(config, token, url, "list")).await.unwrap()).unwrap();
    assert_eq!(output["projects"], json!(projects));
    assert_eq!(output["projects"].as_array().unwrap().len(), 100);
    assert_eq!(output["matched_count"], 137);
    assert_eq!(output["truncated"], true);
    let (path, body) = thread.join().unwrap();
    assert_eq!(path, "POST /api/projects/list HTTP/1.1");
    // Preserve the exact filter; do not invent a larger server page or pagination.
    assert_eq!(body, json!({"client_id":"runner"}));
}

#[tokio::test]
async fn project_management_missing_legacy_inventory_metadata_stays_unknown() {
    for (matched_count, truncated) in [(None, None), (Some(37u64), None), (None, Some(true))] {
        let mut inventory = json!({"projects":[inventory_project(0)]});
        if let Some(count) = matched_count {
            inventory["matched_count"] = json!(count);
        }
        if let Some(truncated) = truncated {
            inventory["truncated"] = json!(truncated);
        }
        let (url, thread) = server(200, json!({"success":true,"output":inventory}));
        let temp = tempfile::tempdir().unwrap();
        let (config, token) = files(&temp, &url);
        let output: Value =
            serde_json::from_str(&run(options(config, token, url, "list")).await.unwrap()).unwrap();
        assert_eq!(output["projects"], json!([inventory_project(0)]));
        assert_eq!(output["matched_count"], json!(matched_count));
        assert_eq!(output["truncated"], json!(truncated));
        assert!(output.get("matched_count").is_some());
        assert!(output.get("truncated").is_some());
        let (_, body) = thread.join().unwrap();
        assert_eq!(body, json!({"client_id":"runner"}));
    }
}

#[tokio::test]
async fn project_management_rejects_present_invalid_inventory_metadata() {
    for (field, invalid_values) in [
        (
            "operator_user_id",
            vec![
                json!(null),
                json!(""),
                json!(" \t"),
                json!("bad\0id"),
                json!("u".repeat(257)),
                json!(123),
            ],
        ),
        (
            "matched_count",
            vec![
                json!(null),
                json!("1"),
                json!(true),
                json!(-1),
                json!(1.5),
                json!([]),
                json!({}),
            ],
        ),
        (
            "truncated",
            vec![
                json!(null),
                json!("false"),
                json!(0),
                json!(1),
                json!([]),
                json!({}),
            ],
        ),
    ] {
        for invalid in invalid_values {
            let mut inventory = json!({"projects":[inventory_project(0)],"matched_count":1,"truncated":false,
                "token":"private-response-value"});
            inventory[field] = invalid;
            let (url, thread) = server(200, json!({"success":true,"output":inventory}));
            let temp = tempfile::tempdir().unwrap();
            let (config, token) = files(&temp, &url);
            let error = run(options(config, token, url, "list")).await.unwrap_err();
            assert_eq!(
                error,
                format!("project_operation_indeterminate: Invalid inventory {field} metadata")
            );
            assert!(!error.contains("private-response-value"));
            assert!(!error.contains("fixture-credential"));
            let (_, body) = thread.join().unwrap();
            assert_eq!(body, json!({"client_id":"runner"}));
        }
    }
}

#[tokio::test]
async fn project_management_metadata_does_not_weaken_project_schema_or_runner_scope() {
    for foreign in [false, true] {
        let mut project = inventory_project(0);
        if foreign {
            project["id"] = json!("agent:other:repo");
        } else {
            project.as_object_mut().unwrap().remove("revision");
        }
        let (url, thread) = server(
            200,
            json!({
                "success":true,"output":{"projects":[project],"matched_count":1,"truncated":false}
            }),
        );
        let temp = tempfile::tempdir().unwrap();
        let (config, token) = files(&temp, &url);
        let error = run(options(config, token, url, "list")).await.unwrap_err();
        assert_eq!(
            error,
            if foreign {
                "project_connection_changed: Inventory belongs to another Runner"
            } else {
                "project_operation_indeterminate: Invalid project inventory"
            }
        );
        let (_, body) = thread.join().unwrap();
        assert_eq!(body, json!({"client_id":"runner"}));
    }
}

#[tokio::test]
async fn project_management_mutations_use_admin_contract_and_original_revision() {
    for action in ["enable", "disable", "unregister"] {
        let (url, thread) = server(
            200,
            json!({"operation":action,"project":"agent:runner:repo","changed":true,"outcome":"done"}),
        );
        let temp = tempfile::tempdir().unwrap();
        let (config, token) = files(&temp, &url);
        let before = std::fs::read(&config).unwrap();
        run(options(config.clone(), token, url, action))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(config).unwrap(),
            before,
            "CLI must not mutate registry/config directly"
        );
        let (path, body) = thread.join().unwrap();
        assert_eq!(path, format!("POST /api/admin/projects/{action} HTTP/1.1"));
        assert_eq!(body["project"], "agent:runner:repo");
        assert_eq!(
            body["expected_revision"],
            format!("sha256:{}", "a".repeat(64))
        );
        assert_eq!(body["idempotency_key"], "same-request");
        assert_eq!(body["confirm"], true);
    }
}

#[tokio::test]
async fn project_management_preserves_rejections_and_unknown_outcomes() {
    for (status, code) in [
        (403, "forbidden"),
        (409, "revision_conflict"),
        (409, "active_jobs_conflict"),
        (503, "operation_indeterminate"),
    ] {
        let (url, thread) = server(
            status,
            json!({"error":{"code":code,"message":"private-response-value"}}),
        );
        let temp = tempfile::tempdir().unwrap();
        let (config, token) = files(&temp, &url);
        let error = run(options(config, token, url, "unregister"))
            .await
            .unwrap_err();
        assert!(error.starts_with(&format!("project_{code}:")), "{error}");
        assert!(!error.contains("private-response-value"));
        thread.join().unwrap();
    }
}

#[tokio::test]
async fn project_management_rejects_retargeting_before_sending_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let (config, token) = files(&temp, "http://127.0.0.1:9");
    for change_server in [true, false] {
        let mut opts = options(
            config.clone(),
            token.clone(),
            "http://127.0.0.1:9".into(),
            "disable",
        );
        if change_server {
            opts.server_url = "https://untrusted.example".into();
        } else {
            opts.project = Some("agent:other:repo".into());
        }
        assert!(run(opts)
            .await
            .unwrap_err()
            .starts_with("project_connection_changed:"));
    }
}

#[test]
fn project_management_parser_requires_exact_identity_and_mutation_receipt_fields() {
    let base = [
        "list",
        "--config",
        "runner.toml",
        "--server-url",
        "http://127.0.0.1:9",
        "--client-id",
        "runner",
        "--token-file",
        "credential",
        "--json",
    ]
    .map(str::to_owned)
    .to_vec();
    assert!(parse(&base).is_ok());
    for action in ["enable", "disable", "unregister"] {
        let mut args = base.clone();
        args[0] = action.into();
        assert!(parse(&args).is_err());
        args.extend(
            [
                "--project",
                "agent:runner:repo",
                "--expected-revision",
                "sha256:test",
                "--idempotency-key",
                "once",
            ]
            .map(str::to_owned),
        );
        assert!(parse(&args).is_ok());
        args.extend(["--project", "agent:other:repo"].map(str::to_owned));
        assert!(parse(&args).is_err());
    }
}
