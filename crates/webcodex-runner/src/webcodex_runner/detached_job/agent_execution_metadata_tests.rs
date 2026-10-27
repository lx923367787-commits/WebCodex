//! Filesystem-only metadata tests; no payload, supervisor, or business process.
use super::*;
use serde_json::json;
use webcodex_core::runner_protocol::{compute_members_sha256, AgentExecutionCloseRef, AgentRunRef};

fn job(index: usize) -> AgentExecutionJobMetadata {
    let binding = serde_json::from_value(json!({
        "version": 1,
        "project": "project",
        "requester": {
            "enrollment_id": "requester-enrollment",
            "run": {"agent_id": "requester", "agent_run_id": "requester-run", "generation": 1},
            "root_task_id": "root-task",
            "source_task_id": "source-task",
            "call_index": 0,
            "cli_attempt_id": format!("attempt-{index:03}")
        },
        "executor": {
            "enrollment_id": "worker-enrollment",
            "run": {"agent_id": "worker", "agent_run_id": "worker-run", "generation": 1},
            "task_id": "worker-task",
            "cli_session_scope_id": "worker-session"
        },
        "target": {"client_id": "runner", "runner_instance_id": "instance"},
        "carrier": {"kind": "job", "job_id": format!("job-{index}"), "start_request_id": format!("request-{index}")},
        "launch_intent_hash": "a".repeat(64),
        "resource_manifest_hash": "b".repeat(64)
    }))
    .unwrap();
    AgentExecutionJobMetadata {
        workspace_snapshot: None,
        binding,
        resource_set_id: "resource-profile".into(),
        native_create_attempted: false,
        worker_finished: false,
        stop_receipt: None,
        resource_cleanup: None,
    }
}

fn stop_receipt(job: &AgentExecutionJobMetadata) -> AgentExecutionStopReceipt {
    AgentExecutionStopReceipt {
        binding_sha256: job.binding.binding_hash().unwrap(),
        carrier: job.binding.carrier.clone(),
        observation_revision: 1,
        outcome: if job.native_create_attempted {
            AgentExecutionStopOutcome::TreeExited {
                leader_exit_code: Some(0),
                leader_handle_signaled: true,
                membership_complete: true,
                member_handles_signaled: true,
                worker_finished: true,
            }
        } else {
            AgentExecutionStopOutcome::NeverCreated {
                request_cancelled: true,
                worker_finished: true,
            }
        },
    }
}

fn close(index: usize, members: Vec<AgentExecutionBinding>) -> AgentExecutionCloseMetadata {
    AgentExecutionCloseMetadata {
        close: AgentExecutionClose {
            version: 1,
            close_ref: AgentExecutionCloseRef {
                close_id: format!("close-{index}"),
                // An ancestor need not be a direct requester/executor member.
                source: AgentRunRef {
                    agent_id: "ancestor".into(),
                    agent_run_id: "ancestor-run".into(),
                    generation: 1,
                },
            },
            target: job(0).binding.target,
            deadline_at_ms: 1_000_000,
            members_sha256: compute_members_sha256(&members).unwrap(),
            members,
        },
        receipt: None,
    }
}

fn close_receipt(close: &AgentExecutionClose) -> AgentExecutionCloseReceipt {
    AgentExecutionCloseReceipt {
        version: 1,
        close_ref: close.close_ref.clone(),
        target: close.target.clone(),
        deadline_at_ms: close.deadline_at_ms,
        members_sha256: close.members_sha256.clone(),
        observation_revision: 1,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: close
            .members
            .iter()
            .map(|binding| {
                let mut local = job(0);
                local.binding = binding.clone();
                stop_receipt(&local)
            })
            .collect(),
    }
}

fn initialized_store() -> (tempfile::TempDir, DetachedJobStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = DetachedJobStore::new(temp.path().join("state"));
    store.initialize_agent_execution_metadata().unwrap();
    (temp, store)
}

#[test]
fn legacy_cleanup_absence_deserializes_without_claiming_resources() {
    let mut encoded = serde_json::to_value(job(0)).unwrap();
    encoded.as_object_mut().unwrap().remove("resource_cleanup");
    let legacy: AgentExecutionJobMetadata = serde_json::from_value(encoded).unwrap();
    assert!(legacy.resource_cleanup.is_none());
    let (_temporary, store) = initialized_store();
    store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs.push(legacy);
            Ok(())
        })
        .unwrap();
    assert!(store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs[0].resource_cleanup = Some(Default::default());
            Ok(())
        })
        .is_err());
    assert!(store.read_agent_execution_metadata().unwrap().jobs[0]
        .resource_cleanup
        .is_none());
}

#[test]
fn cleanup_ownership_and_progress_cannot_be_removed_replaced_or_regressed() {
    let (_temporary, store) = initialized_store();
    let mut record = job(0);
    record.resource_cleanup = Some(AgentExecutionResourceCleanupMetadata {
        auth_copy_expected: true,
        proxy_requested: true,
        ..Default::default()
    });
    store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs.push(record);
            Ok(())
        })
        .unwrap();
    store
        .update_agent_execution_metadata(|metadata| {
            let record = &mut metadata.jobs[0];
            record.native_create_attempted = true;
            record.worker_finished = true;
            record.stop_receipt = Some(stop_receipt(record));
            let cleanup = record.resource_cleanup.as_mut().unwrap();
            cleanup.auth_removed = true;
            cleanup.grant_intent = true;
            cleanup.grant_verified = true;
            cleanup.proxy_revoked = true;
            cleanup.resources_stopped = true;
            Ok(())
        })
        .unwrap();
    let complete = store.read_agent_execution_metadata().unwrap();
    let mutations: &[fn(&mut AgentExecutionJobMetadata)] = &[
        |job| job.resource_cleanup = None,
        |job| job.resource_cleanup = Some(Default::default()),
        |job| job.resource_cleanup.as_mut().unwrap().auth_copy_expected = false,
        |job| job.resource_cleanup.as_mut().unwrap().proxy_requested = false,
        |job| job.resource_cleanup.as_mut().unwrap().auth_removed = false,
        |job| job.resource_cleanup.as_mut().unwrap().grant_intent = false,
        |job| job.resource_cleanup.as_mut().unwrap().grant_verified = false,
        |job| job.resource_cleanup.as_mut().unwrap().proxy_revoked = false,
        |job| job.resource_cleanup.as_mut().unwrap().resources_stopped = false,
    ];
    for mutate in mutations {
        assert!(store
            .update_agent_execution_metadata(|metadata| {
                mutate(&mut metadata.jobs[0]);
                Ok(())
            })
            .is_err());
        assert_eq!(store.read_agent_execution_metadata().unwrap(), complete);
    }
}

#[test]
fn cleanup_flags_require_owned_grant_and_complete_stop_evidence() {
    let mutations: &[fn(&mut AgentExecutionResourceCleanupMetadata)] = &[
        |cleanup| cleanup.resources_stopped = true,
        |cleanup| cleanup.auth_removed = true,
        |cleanup| cleanup.grant_intent = true,
        |cleanup| cleanup.grant_verified = true,
        |cleanup| cleanup.proxy_revoked = true,
        |cleanup| cleanup.last_cleanup_error = Some("private proxy value".into()),
    ];
    for mutate in mutations {
        let mut record = job(0);
        let mut cleanup = AgentExecutionResourceCleanupMetadata::default();
        mutate(&mut cleanup);
        record.resource_cleanup = Some(cleanup);
        let metadata = AgentExecutionMetadata {
            jobs: vec![record],
            ..Default::default()
        };
        assert!(validate_agent_execution_metadata(&metadata).is_err());
    }
}

#[test]
fn bootstrap_is_explicit_and_restart_preserves_an_unknown_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let store = DetachedJobStore::new(temp.path().join("state"));
    assert!(store.read_agent_execution_metadata().is_err());
    assert!(store.update_agent_execution_metadata(|_| Ok(())).is_err());
    assert_eq!(
        store.initialize_agent_execution_metadata().unwrap(),
        AgentExecutionMetadata::default()
    );
    let value = store
        .update_agent_execution_metadata(|metadata| {
            let mut admitted = job(0);
            admitted.native_create_attempted = true;
            metadata.jobs.push(admitted);
            Ok(17)
        })
        .unwrap();
    assert_eq!(value, 17);
    let restarted = DetachedJobStore::new(store.root.clone());
    let durable = restarted.read_agent_execution_metadata().unwrap();
    assert!(durable.jobs[0].native_create_attempted);
    assert!(!durable.jobs[0].worker_finished);
    assert!(durable.jobs[0].stop_receipt.is_none());
    assert_eq!(
        restarted.initialize_agent_execution_metadata().unwrap(),
        durable
    );
    fs::remove_file(store.root.join(AGENT_EXECUTION_METADATA_FILE)).unwrap();
    assert!(restarted.read_agent_execution_metadata().is_err());
    assert!(restarted
        .update_agent_execution_metadata(|_| Ok(()))
        .is_err());
}

#[test]
fn failed_closure_or_atomic_commit_does_not_replace_durable_metadata() {
    let (_temp, store) = initialized_store();
    let original = fs::read(store.root.join(AGENT_EXECUTION_METADATA_FILE)).unwrap();
    let result: Result<(), String> = store.update_agent_execution_metadata(|metadata| {
        metadata.jobs.push(job(0));
        Err("abort".into())
    });
    assert_eq!(result.unwrap_err(), "abort");
    fs::create_dir(store.root.join(STATE_TEMP_FILE)).unwrap();
    assert!(store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs.push(job(0));
            Ok(())
        })
        .is_err());
    assert_eq!(
        fs::read(store.root.join(AGENT_EXECUTION_METADATA_FILE)).unwrap(),
        original
    );
}

#[test]
fn job_identity_flags_and_stop_receipts_are_monotonic() {
    let (_temp, store) = initialized_store();
    store
        .update_agent_execution_metadata(|metadata| {
            let mut entry = job(0);
            entry.native_create_attempted = true;
            metadata.jobs.push(entry);
            Ok(())
        })
        .unwrap();
    let invalid_changes: &[fn(&mut AgentExecutionMetadata)] = &[
        |m| m.jobs.clear(),
        |m| m.jobs[0].binding.requester.cli_attempt_id = "replacement".into(),
        |m| m.jobs[0].binding.launch_intent_hash = "c".repeat(64),
        |m| m.jobs[0].resource_set_id = "replacement".into(),
        |m| m.jobs[0].native_create_attempted = false,
    ];
    for change in invalid_changes {
        let original = store.read_agent_execution_metadata().unwrap();
        assert!(store
            .update_agent_execution_metadata(|metadata| {
                change(metadata);
                Ok(())
            })
            .is_err());
        assert_eq!(store.read_agent_execution_metadata().unwrap(), original);
    }
    store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs[0].worker_finished = true;
            metadata.jobs[0].stop_receipt = Some(stop_receipt(&metadata.jobs[0]));
            Ok(())
        })
        .unwrap();
    let invalid_changes: &[fn(&mut AgentExecutionMetadata)] = &[
        |m| m.jobs[0].worker_finished = false,
        |m| m.jobs[0].stop_receipt = None,
        |m| {
            m.jobs[0]
                .stop_receipt
                .as_mut()
                .unwrap()
                .observation_revision += 1
        },
    ];
    for change in invalid_changes {
        assert!(store
            .update_agent_execution_metadata(|metadata| {
                change(metadata);
                Ok(())
            })
            .is_err());
    }
    store.update_agent_execution_metadata(|_| Ok(())).unwrap();
}

#[test]
fn stop_proofs_require_exact_identity_and_observed_native_creation_state() {
    for attempted in [false, true] {
        let mut entry = job(0);
        entry.native_create_attempted = attempted;
        entry.worker_finished = true;
        entry.stop_receipt = Some(stop_receipt(&entry));
        let valid = AgentExecutionMetadata {
            jobs: vec![entry],
            ..Default::default()
        };
        validate_agent_execution_metadata(&valid).unwrap();
        let mutations: &[fn(&mut AgentExecutionJobMetadata)] = &[
            |j| j.worker_finished = false,
            |j| j.native_create_attempted = !j.native_create_attempted,
            |j| j.stop_receipt.as_mut().unwrap().binding_sha256 = "c".repeat(64),
            |j| j.stop_receipt.as_mut().unwrap().carrier = job(1).binding.carrier,
            |j| j.stop_receipt.as_mut().unwrap().observation_revision = 0,
        ];
        for mutation in mutations {
            let mut invalid = valid.clone();
            mutation(&mut invalid.jobs[0]);
            assert!(validate_agent_execution_metadata(&invalid).is_err());
        }
        let outcome =
            serde_json::to_value(&valid.jobs[0].stop_receipt.as_ref().unwrap().outcome).unwrap();
        for (name, value) in outcome.as_object().unwrap() {
            if value.is_boolean() {
                let mut invalid = valid.clone();
                let mut changed = outcome.clone();
                changed[name] = json!(false);
                invalid.jobs[0].stop_receipt.as_mut().unwrap().outcome =
                    serde_json::from_value(changed).unwrap();
                assert!(
                    validate_agent_execution_metadata(&invalid).is_err(),
                    "{name}"
                );
            }
        }
    }
}

#[test]
fn close_can_precede_start_but_identity_and_receipt_cannot_change() {
    let (_temp, store) = initialized_store();
    store
        .update_agent_execution_metadata(|metadata| {
            metadata.closes.push(close(0, vec![job(0).binding]));
            Ok(())
        })
        .unwrap();
    assert!(store
        .read_agent_execution_metadata()
        .unwrap()
        .jobs
        .is_empty());
    let mutations: &[fn(&mut AgentExecutionMetadata)] = &[
        |m| m.closes.clear(),
        |m| m.closes[0].close.close_ref.close_id = "replacement".into(),
        |m| m.closes[0].close.close_ref.source.agent_id = "replacement".into(),
        |m| m.closes[0].close.deadline_at_ms += 1,
    ];
    for mutation in mutations {
        assert!(store
            .update_agent_execution_metadata(|metadata| {
                mutation(metadata);
                Ok(())
            })
            .is_err());
    }
    store
        .update_agent_execution_metadata(|metadata| {
            metadata.closes[0].receipt = Some(close_receipt(&metadata.closes[0].close));
            Ok(())
        })
        .unwrap();
    let committed = store.read_agent_execution_metadata().unwrap();
    for remove in [false, true] {
        assert!(store
            .update_agent_execution_metadata(|metadata| {
                if remove {
                    metadata.closes[0].receipt = None;
                } else {
                    metadata.closes[0]
                        .receipt
                        .as_mut()
                        .unwrap()
                        .observation_revision += 1;
                }
                Ok(())
            })
            .is_err());
    }
    assert_eq!(store.read_agent_execution_metadata().unwrap(), committed);
    store
        .update_agent_execution_metadata(|metadata| {
            *metadata = committed;
            Ok(())
        })
        .unwrap();
}

#[test]
fn metadata_rejects_duplicate_identities_and_bounded_record_or_byte_overflow() {
    let duplicate_jobs = AgentExecutionMetadata {
        jobs: vec![job(0), job(0)],
        ..Default::default()
    };
    assert!(validate_agent_execution_metadata(&duplicate_jobs).is_err());
    let duplicate_closes = AgentExecutionMetadata {
        closes: vec![close(0, Vec::new()), close(0, Vec::new())],
        ..Default::default()
    };
    assert!(validate_agent_execution_metadata(&duplicate_closes).is_err());
    for too_many_jobs in [false, true] {
        let mut metadata = AgentExecutionMetadata::default();
        if too_many_jobs {
            metadata.jobs = (0..129).map(job).collect();
        } else {
            metadata.closes = (0..129).map(|i| close(i, Vec::new())).collect();
        }
        assert_eq!(
            validate_agent_execution_metadata(&metadata).unwrap_err(),
            "agent_execution_metadata_capacity_exceeded"
        );
    }
    let members: Vec<_> = (0..16).map(|i| job(i).binding).collect();
    let oversized = AgentExecutionMetadata {
        closes: (0..128).map(|i| close(i, members.clone())).collect(),
        ..Default::default()
    };
    assert!(serde_json::to_vec(&oversized).unwrap().len() > DETACHED_STATE_MAX_BYTES);
    assert_eq!(
        validate_agent_execution_metadata(&oversized).unwrap_err(),
        "agent_execution_metadata_too_large"
    );
}

#[test]
fn corrupt_unknown_or_oversized_metadata_is_never_reset_by_bootstrap() {
    let (_temp, store) = initialized_store();
    let path = store.root.join(AGENT_EXECUTION_METADATA_FILE);
    let mut unknown = serde_json::to_value(AgentExecutionMetadata::default()).unwrap();
    unknown["argv"] = json!(["forbidden"]);
    let mut nested_unknown = serde_json::to_value(AgentExecutionMetadata {
        jobs: vec![job(0)],
        ..Default::default()
    })
    .unwrap();
    nested_unknown["jobs"][0]["stdin"] = json!("forbidden");
    for bytes in [
        b"{".to_vec(),
        serde_json::to_vec(&unknown).unwrap(),
        serde_json::to_vec(&nested_unknown).unwrap(),
        vec![b' '; DETACHED_STATE_MAX_BYTES + 1],
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(store.read_agent_execution_metadata().is_err());
        assert!(store.initialize_agent_execution_metadata().is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn detached_scanning_and_reclamation_preserve_metadata_and_crash_temp() {
    let (_temp, store) = initialized_store();
    store
        .update_agent_execution_metadata(|metadata| {
            metadata.closes.push(close(0, Vec::new()));
            Ok(())
        })
        .unwrap();
    let metadata = store.read_agent_execution_metadata().unwrap();
    let request = DetachedStartRequest {
        job_id: "detached-job".into(),
        request_id: "detached-request".into(),
        client_id: "runner".into(),
        runner_instance_id: "instance".into(),
        context: serde_json::from_value(json!({"command_preview": "filesystem fixture"})).unwrap(),
        launch: DetachedLaunchSpec {
            process: ShellProcessArgv {
                executable: "never-launched".into(),
                args: Vec::new(),
            },
            cwd: None,
            stdin: None,
            env: Vec::new(),
            timeout_secs: 30,
        },
    };
    let PrepareOutcome::First(prepared) = store.prepare(&request).unwrap() else {
        panic!("fresh fixture must prepare once");
    };
    let terminal = store
        .update(&request.job_id, &prepared.execution_id, |record| {
            set_terminal(
                record,
                "completed",
                Some(0),
                None,
                record.created_at_unix_ms,
            );
            Ok(())
        })
        .unwrap();
    fs::write(store.root.join(STATE_TEMP_FILE), b"partial metadata").unwrap();
    assert_eq!(
        store.scan_for_client("runner").unwrap(),
        vec![terminal.clone()]
    );
    let completed = terminal.terminal.unwrap().completed_at_unix_ms;
    {
        let _root_lock = exclusive_lock(&store.root.join(ROOT_LOCK_FILE), true).unwrap();
        assert_eq!(
            store
                .reclaim_expired_terminal_records_locked(completed + TERMINAL_RETENTION_MS)
                .unwrap(),
            0
        );
    }
    assert!(store.scan_for_client("runner").unwrap().is_empty());
    assert_eq!(store.read_agent_execution_metadata().unwrap(), metadata);
    assert!(store.root.join(STATE_TEMP_FILE).is_file());
    fs::write(store.root.join(".unrecognized"), b"unknown").unwrap();
    assert!(store.scan_for_client("runner").is_err());
    assert!(store
        .reclaim_expired_terminal_records_locked(unix_ms())
        .is_err());
}

#[test]
fn reserved_root_names_must_be_regular_files() {
    let (_temp, store) = initialized_store();
    fs::create_dir(store.root.join(STATE_TEMP_FILE)).unwrap();
    assert!(store.scan_for_client("runner").is_err());
    assert!(store
        .reclaim_expired_terminal_records_locked(unix_ms())
        .is_err());
}

#[cfg(unix)]
#[test]
fn metadata_and_crash_temp_symlinks_fail_closed_without_touching_targets() {
    use std::os::unix::fs::symlink;

    for name in [AGENT_EXECUTION_METADATA_FILE, STATE_TEMP_FILE] {
        let (temp, store) = initialized_store();
        let external = temp.path().join("external");
        fs::write(&external, b"untouched").unwrap();
        let path = store.root.join(name);
        if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        symlink(&external, &path).unwrap();
        assert!(store.scan_for_client("runner").is_err());
        assert!(store
            .reclaim_expired_terminal_records_locked(unix_ms())
            .is_err());
        if name == AGENT_EXECUTION_METADATA_FILE {
            assert!(store.initialize_agent_execution_metadata().is_err());
            assert!(store.read_agent_execution_metadata().is_err());
        } else {
            assert!(store
                .update_agent_execution_metadata(|metadata| {
                    metadata.jobs.push(job(0));
                    Ok(())
                })
                .is_err());
        }
        assert_eq!(fs::read(external).unwrap(), b"untouched");
    }
}
