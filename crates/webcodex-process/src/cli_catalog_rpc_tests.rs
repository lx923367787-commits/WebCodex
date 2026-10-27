use super::*;
use std::io::Cursor;

#[test]
fn codex_pages_keep_model_specific_efforts_and_send_no_turn() {
    let input = concat!(
        "{\"id\":1,\"result\":{}}\n",
        "{\"method\":\"notice\",\"params\":{}}\n",
        "{\"id\":2,\"result\":{\"data\":[{\"model\":\"model-a\",\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"high\"}]}],\"nextCursor\":\"page-2\"}}\n",
        "{\"id\":3,\"result\":{\"data\":[{\"model\":\"model-b\",\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"low\"}]}],\"nextCursor\":null}}\n"
    );
    let mut sent = Vec::new();
    let out = exchange(Protocol::Codex, Cursor::new(input), &mut sent).unwrap();
    let models = parse_models(std::str::from_utf8(&out).unwrap()).unwrap();
    assert_eq!(models["model-a"], ["high"]);
    assert_eq!(models["model-b"], ["low"]);
    let requests: Vec<Value> = String::from_utf8(sent)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|r| matches!(
        r["method"].as_str(),
        Some("initialize" | "initialized" | "model/list")
    )));
    assert_eq!(requests[3]["params"]["cursor"], "page-2");
}

#[test]
fn claude_models_do_not_leak_account_fields_or_invent_effort() {
    let response = json!({"type":"control_response","response":{"subtype":"success","request_id":"webcodex_catalog","response":{"account":{"secret":"not projected"},"models":[{"value":"default","resolvedModel":"model-a[1m]","supportsEffort":true,"supportedEffortLevels":["low","high"]},{"value":"alias","resolvedModel":"model-a[1m]","supportsEffort":true,"supportedEffortLevels":["low","high"]},{"value":"model-b","supportsEffort":false}]}}});
    let mut sent = Vec::new();
    let output = exchange(
        Protocol::Claude,
        Cursor::new(format!("{response}\n")),
        &mut sent,
    )
    .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains("secret"));
    assert!(!text.contains("default"));
    assert_eq!(parse_models(&text).unwrap()["model-a[1m]"], ["low", "high"]);
    assert_eq!(
        parse_models(&text).unwrap()["model-b"],
        Vec::<String>::new()
    );
    assert!(!String::from_utf8(sent).unwrap().contains("user"));
}

#[test]
fn malformed_duplicate_oversize_and_unsolicited_requests_fail_closed() {
    for response in [
        json!({"id":7,"method":"requestApproval"}),
        json!({"id":1,"error":{"message":"private account data"}}),
        json!({"id":1,"result":{}}),
    ] {
        let error = exchange(
            Protocol::Codex,
            Cursor::new(format!("{response}\n")),
            Vec::new(),
        )
        .unwrap_err();
        assert!(!error.contains("private"));
    }
    assert!(exchange(
        Protocol::Claude,
        Cursor::new(vec![b' '; OUTPUT_LIMIT + 1]),
        Vec::new()
    )
    .is_err());
    let rows = json!([{"model":"same","supportedReasoningEfforts":[]},{"model":"same","supportedReasoningEfforts":[]}]);
    assert!(add_models(&mut BTreeMap::new(), &rows, Protocol::Codex).is_err());
    let rows = json!([{"value":"unknown-capabilities","supportsEffort":true}]);
    assert!(add_models(&mut BTreeMap::new(), &rows, Protocol::Claude).is_err());
}
