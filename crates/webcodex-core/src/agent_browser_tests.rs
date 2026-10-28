use super::*;
#[test]
fn browser_steps_cannot_select_host_code_paths_or_other_origins() {
    for path in [
        "https://example.test/",
        "//other/",
        "/\\other",
        "file:///private",
        "/\n",
    ] {
        assert!(validate_steps(&[BrowserStep::Navigate { path: path.into() }]).is_err());
    }
    assert!(validate_steps(&[
        BrowserStep::Navigate {
            path: "/app?simulation=1".into()
        },
        BrowserStep::Fill {
            selector: "#name".into(),
            text: "模拟身份".into()
        },
        BrowserStep::Screenshot {}
    ])
    .is_ok());
    assert!(validate_steps(&[BrowserStep::Click {
        accept_dialog: false,
        selector: "body >> xpath=//*".into()
    }])
    .is_err());
    for value in [
        serde_json::json!({"action":"evaluate","script":"process.exit()"}),
        serde_json::json!({"action":"screenshot","path":"C:/other/private.png"}),
        serde_json::json!({"action":"navigate","path":"/","url":"https://other/"}),
    ] {
        assert!(serde_json::from_value::<BrowserStep>(value).is_err());
    }
}
