use super::*;

#[tokio::test]
async fn enrollment_http_collaboration_restores_events_without_granting_control_or_foreign_access()
{
    let f = Fixture::new();
    let request = f.reservation("event-manager", "event-manager");
    let grant = f.reserve(request).await;
    let attached = f.attach("event-window", &grant).await;
    let task = attached["assignment"]["task_id"].as_str().unwrap();
    let event =
        f.db.publish_team_framework_event(
            &f.user.id,
            PROJECT,
            task,
            "fixture-build",
            "Framework capability changed; original task authorization still applies.",
            vec![task.into()],
            "fixture-build",
        )
        .unwrap();
    let read = success(
        f.call(
            "event-window",
            "team_collaboration",
            json!({"action":"read"}),
        )
        .await,
    );
    assert_eq!(read["events"][0]["sequence"], event["sequence"]);
    assert_eq!(read["events"][0]["origin"], "framework");
    let waiting=success(f.call("event-window","team_collaboration",json!({"action":"wait","reason":"Waiting for public dependency","through_sequence":read["next_sequence"]})).await);
    assert_eq!(waiting["execution_state"], "waiting_dependencies");
    denied(
        &f.call(
            "event-window",
            "team_collaboration",
            json!({"action":"read","agent_id":"another-agent"}),
        )
        .await,
    );
    let (code,_)=f.control(&f.oauth,json!({"action":"team_continuation","operation":"list","owner_user":f.user.id,"project":PROJECT,"host_id":"forged"})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let second = f
        .reserve(f.reservation("other-manager", "other-manager"))
        .await;
    f.attach("other-window", &second).await;
    let other = success(
        f.call(
            "other-window",
            "team_collaboration",
            json!({"action":"read"}),
        )
        .await,
    );
    assert!(other["events"].as_array().unwrap().is_empty());
}
