//! Review evidence shares goal/attempt authority; it never grants execution.
use super::*;
use std::collections::BTreeSet;
use webcodex_core::goal::GoalArtifactSnapshot;

fn review_read(
    conn: &Connection,
    goal_id: &str,
    node_id: &str,
) -> Result<Option<HostGoalReviewRecord>, GoalStoreError> {
    conn.query_row(
        "SELECT snapshot_json FROM wc_host_goal_reviews WHERE goal_id=?1 AND node_id=?2",
        params![goal_id, node_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(unavailable)?
    .map(decode)
    .transpose()
}

fn review_save(conn: &Connection, record: &HostGoalReviewRecord) -> Result<(), GoalStoreError> {
    let encoded = encode(record)?;
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(invalid("Review evidence exceeds the bounded 4 MiB archive"));
    }
    conn.execute("INSERT INTO wc_host_goal_reviews(goal_id,node_id,snapshot_json) VALUES(?1,?2,?3) ON CONFLICT(goal_id,node_id) DO UPDATE SET snapshot_json=excluded.snapshot_json",params![record.goal_id,record.node_id,encoded]).map_err(unavailable)?;
    Ok(())
}

fn archive_current_packet(record: &mut HostGoalReviewRecord) -> Result<(), GoalStoreError> {
    if let Some(packet) = &record.current_packet {
        if !record
            .rounds
            .iter()
            .any(|round| round.packet.packet_id == packet.packet_id)
        {
            if record.superseded_packets.len() >= 32 {
                return Err(conflict("Prepared material archive reached its bounded limit; retain records and report the blocked delivery"));
            }
            record.superseded_packets.push(packet.clone());
        }
    }
    record.current_packet = None;
    Ok(())
}

fn fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_manifest(manifest: &[GoalArtifactSnapshot]) -> Result<(), GoalStoreError> {
    if manifest.len() > 64 {
        return Err(invalid("Review manifest exceeds 64 files"));
    }
    let mut paths = BTreeSet::new();
    for artifact in manifest {
        webcodex_core::goal::validate_goal_relative_path(&artifact.path).map_err(invalid)?;
        if !paths.insert(&artifact.path) || !artifact.sha256.as_deref().is_some_and(fingerprint) {
            return Err(invalid(
                "Review requires unique existing files with SHA256 fingerprints",
            ));
        }
    }
    Ok(())
}

fn validate_material(
    material: &HostGoalReviewMaterial,
    manifest: &[GoalArtifactSnapshot],
    state_sha256: Option<&str>,
) -> Result<(), GoalStoreError> {
    bounded(&material.id, 1200, "review material scope")?;
    bounded(&material.path, 1024, "review source path")?;
    let source_matches = if material.kind == "state" {
        material.path == "goal-state" && state_sha256 == Some(material.sha256.as_str())
    } else {
        manifest.iter().any(|artifact| {
            artifact.path == material.path
                && artifact.sha256.as_deref() == Some(material.sha256.as_str())
        })
    };
    if !fingerprint(&material.sha256) || !source_matches {
        return Err(invalid(
            "Review material must match its actual source in the prepared manifest",
        ));
    }
    match material.kind.as_str() {
        "image" => {
            if material.render_source_sha256.as_deref() != Some(material.sha256.as_str())
                || !material.preview_sha256.as_deref().is_some_and(fingerprint)
                || !material
                    .render_dependencies_sha256
                    .as_deref()
                    .is_some_and(fingerprint)
                || material
                    .preview_path
                    .as_ref()
                    .is_none_or(|path| path.is_empty() || path.len() > 4096)
            {
                return Err(invalid("Image evidence requires source-bound preview and rendering dependency fingerprints"));
            }
        }
        "text" | "state" => {
            bounded(
                material.content.as_deref().unwrap_or_default(),
                16 * 1024,
                "actual review material",
            )?;
        }
        _ => return Err(invalid("Review material kind must be text, state or image")),
    }
    if material
        .content
        .as_ref()
        .is_some_and(|text| text.len() > 16 * 1024)
    {
        return Err(invalid("Review material text exceeds 16 KiB"));
    }
    Ok(())
}

fn verify_state_material(
    goal: &HostGoalRecord,
    packet: &HostGoalReviewPacket,
) -> Result<(), GoalStoreError> {
    if let Some(expected) = &packet.state_sha256 {
        if !fingerprint(expected)
            || &request_hash(&host_goal_review_state_material(&goal.state))? != expected
        {
            return Err(conflict(
                "Persisted goal results changed since the state review material was prepared",
            ));
        }
        if goal.state.completed_summary.trim().is_empty()
            && goal.state.facts.is_empty()
            && goal.state.decisions.is_empty()
        {
            return Err(invalid(
                "Persist actual text results before requesting state-only review",
            ));
        }
        if !packet
            .materials
            .iter()
            .any(|material| material.kind == "state")
        {
            return Err(invalid(
                "State fingerprint requires actual returned state review material",
            ));
        }
    }
    Ok(())
}

fn validate_scopes(scopes: &[String]) -> Result<(), GoalStoreError> {
    if scopes.len() > 512 {
        return Err(invalid("Review coverage exceeds 512 scopes"));
    }
    let mut seen = BTreeSet::new();
    for scope in scopes {
        bounded(scope, 1200, "review scope")?;
        if !seen.insert(scope) {
            return Err(invalid("Review scopes must be unique"));
        }
    }
    Ok(())
}

fn review_binding(
    conn: &Connection,
    owner: &str,
    binding: &HostGoalExecutionBinding,
    epoch: Option<&str>,
) -> Result<HostGoalRecord, GoalStoreError> {
    let goal = read(conn, owner, &binding.goal_id)?.ok_or_else(not_found)?;
    check_binding(&goal, binding)?;
    let value: String = conn
        .query_row(
            "SELECT snapshot_json FROM wc_host_goal_attempts WHERE goal_id=?1 AND attempt_id=?2",
            params![binding.goal_id, binding.attempt_id],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    let attempt: HostGoalAttempt = decode(value)?;
    if attempt.context_epoch.as_deref() != epoch {
        return Err(conflict("Review belongs to another conversation; prepare and deliver material in the current attempt"));
    }
    Ok(goal)
}

fn layout_paths<'a>(node: &'a HostGoalNode, record: &'a HostGoalReviewRecord) -> Vec<&'a str> {
    node.managed_artifacts
        .as_deref()
        .unwrap_or_default()
        .iter()
        .chain(record.managed_artifacts.iter())
        .filter(|artifact| {
            artifact.kind == HostGoalDeliveryKind::Layout || host_goal_layout_path(&artifact.path)
        })
        .map(|artifact| artifact.path.as_str())
        .collect()
}

pub(super) fn prevent_review_downgrade(
    conn: &Connection,
    goal: &HostGoalRecord,
    old: &HostGoalPlan,
    new: &HostGoalPlan,
) -> Result<(), GoalStoreError> {
    for prior in &old.nodes {
        let record = review_read(conn, &goal.goal_id, &prior.id)?;
        let next = new.nodes.iter().find(|node| node.id == prior.id);
        if record.is_some() && next.is_none() {
            return Err(conflict(
                "A node with archived review evidence cannot be removed to reset review",
            ));
        }
        if let Some(next) = next {
            if host_goal_review_policy(prior).review == 1
                && host_goal_review_policy(next).review == 0
            {
                return Err(conflict(
                    "An established review requirement cannot be downgraded",
                ));
            }
            if let Some(record) = record {
                if host_goal_review_policy(next).max_rounds != record.max_rounds {
                    return Err(conflict(
                        "Review round limit is fixed after material preparation",
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn require_current_pass(
    conn: &Connection,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
    node: &HostGoalNode,
    evidence: &[HostGoalCheckEvidence],
    final_evidence: &[HostGoalCheckEvidence],
    context: Option<&HostGoalInvocationContext>,
) -> Result<(), GoalStoreError> {
    let record = review_read(conn, &goal.goal_id, &node.id)?;
    if host_goal_review_policy(node).review == 0
        && record.as_ref().is_none_or(|record| record.review == 0)
    {
        return Ok(());
    }
    let record = record.ok_or_else(|| {
        conflict("Review is required but has not been recorded; prepare actual materials first")
    })?;
    let packet = record
        .current_packet
        .as_ref()
        .ok_or_else(|| conflict("Review material is not prepared"))?;
    let round = record.rounds.last().filter(|round| round.passed && round.packet == *packet)
        .ok_or_else(|| conflict("Current material has no passing model_self_check; generation alone cannot complete the node"))?;
    if packet.binding != *binding
        || packet.context_epoch.as_deref() != context.map(|value| value.context_epoch.as_str())
    {
        return Err(conflict(
            "Review approval is for another attempt, version or conversation",
        ));
    }
    verify_state_material(goal, packet)?;
    // Compare the original reviewed fingerprint, never restamp a verdict with a
    // newly collected manifest. Runtime also rechecks the actual files.
    for item in evidence.iter().chain(final_evidence) {
        if item.source == HostGoalEvidenceSource::ModelSelfCheck {
            if !round.evidence.contains(item) {
                return Err(conflict(
                    "Acceptance review evidence differs from the archived model verdict",
                ));
            }
        }
        for artifact in &item.artifacts {
            if !packet.manifest.contains(artifact) {
                return Err(conflict(
                    "Acceptance artifact changed after review material was prepared",
                ));
            }
        }
    }
    Ok(())
}

impl Database {
    pub fn enforce_host_goal_layout_review(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        path: &str,
        _now: i64,
    ) -> Result<HostGoalReviewRecord, GoalStoreError> {
        webcodex_core::goal::validate_goal_relative_path(path).map_err(invalid)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let goal = read(&tx, owner, &binding.goal_id)?.ok_or_else(not_found)?;
        check_binding(&goal, binding)?;
        let plan = current_plan(&tx, &goal)?;
        let node = plan
            .plan
            .nodes
            .iter()
            .find(|node| node.id == binding.node_id)
            .ok_or_else(|| invalid("Unknown delivery node"))?;
        let policy = host_goal_review_policy(node);
        let mut record =
            review_read(&tx, &goal.goal_id, &node.id)?.unwrap_or(HostGoalReviewRecord {
                goal_id: goal.goal_id.clone(),
                node_id: node.id.clone(),
                review: 1,
                reason: policy.reason,
                max_rounds: policy.max_rounds,
                managed_artifacts: vec![],
                current_packet: None,
                superseded_packets: vec![],
                rounds: vec![],
            });
        record.review = 1;
        record.reason = "Actual managed document or drawing operation requires source-bound visual review, overriding lightweight classification".into();
        if !record
            .managed_artifacts
            .iter()
            .any(|artifact| artifact.path == path)
        {
            if record.managed_artifacts.len() >= 64 {
                return Err(invalid("Actual managed deliveries exceed 64 files"));
            }
            record.managed_artifacts.push(HostGoalManagedArtifact {
                path: path.into(),
                kind: HostGoalDeliveryKind::Layout,
            });
        }
        // A successful edit invalidates any prior passing packet, including an
        // edit preserving page count. Its original verdict remains in rounds.
        archive_current_packet(&mut record)?;
        review_save(&tx, &record)?;
        tx.commit().map_err(unavailable)?;
        Ok(record)
    }

    pub fn read_host_goal_review(
        &self,
        owner: &str,
        goal_id: &str,
        node_id: &str,
    ) -> Result<Option<HostGoalReviewRecord>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, goal_id)?.ok_or_else(not_found)?;
        review_read(&conn, goal_id, node_id)
    }

    pub fn list_host_goal_reviews(
        &self,
        owner: &str,
        goal_id: &str,
    ) -> Result<Vec<HostGoalReviewRecord>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, goal_id)?.ok_or_else(not_found)?;
        let mut statement = conn.prepare("SELECT snapshot_json FROM wc_host_goal_reviews WHERE goal_id=?1 ORDER BY node_id LIMIT 64").map_err(unavailable)?;
        let rows = statement
            .query_map([goal_id], |row| row.get::<_, String>(0))
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        rows.into_iter().map(decode).collect()
    }

    pub fn prepare_host_goal_review(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        packet: &HostGoalReviewPacket,
        now: i64,
    ) -> Result<HostGoalReviewRecord, GoalStoreError> {
        bounded(&packet.packet_id, 128, "review_packet_id")?;
        validate_manifest(&packet.manifest)?;
        validate_scopes(&packet.required_scopes)?;
        validate_scopes(&packet.delivered_scopes)?;
        if packet.binding != *binding
            || packet.materials.len() > 512
            || packet.machine_checks.len() > 64
        {
            return Err(invalid(
                "Prepared review binding or bounded material is invalid",
            ));
        }
        if packet
            .machine_checks
            .iter()
            .any(|item| !item.passed || item.source != HostGoalEvidenceSource::Automated)
        {
            return Err(conflict(
                "Review material can only be prepared after successful automated checks",
            ));
        }
        for material in &packet.materials {
            validate_material(material, &packet.manifest, packet.state_sha256.as_deref())?;
        }
        if packet.delivered_scopes.iter().any(|scope| {
            !packet
                .materials
                .iter()
                .any(|item| item.id == *scope && matches!(item.kind.as_str(), "text" | "state"))
        }) {
            return Err(invalid("Only actual text material can be delivered by prepare; images need the native delivery receipt"));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let goal = review_binding(&tx, owner, binding, packet.context_epoch.as_deref())?;
        verify_state_material(&goal, packet)?;
        if unresolved(&tx, &goal.goal_id)? {
            return Err(conflict(
                "Resolve pending operations before preparing review",
            ));
        }
        let plan = current_plan(&tx, &goal)?;
        let node = plan
            .plan
            .nodes
            .iter()
            .find(|node| node.id == binding.node_id)
            .ok_or_else(|| invalid("Unknown review node"))?;
        let policy = host_goal_review_policy(node);
        let mut record =
            review_read(&tx, &goal.goal_id, &node.id)?.unwrap_or(HostGoalReviewRecord {
                goal_id: goal.goal_id.clone(),
                node_id: node.id.clone(),
                review: 1,
                reason: policy.reason,
                max_rounds: policy.max_rounds,
                managed_artifacts: vec![],
                current_packet: None,
                superseded_packets: vec![],
                rounds: vec![],
            });
        if policy.review != 1 && record.managed_artifacts.is_empty() {
            return Err(invalid(
                "This lightweight node does not require additional review",
            ));
        }
        for artifact in node
            .managed_artifacts
            .as_deref()
            .unwrap_or_default()
            .iter()
            .chain(record.managed_artifacts.iter())
        {
            if !packet
                .manifest
                .iter()
                .any(|item| item.path == artifact.path)
            {
                return Err(invalid("Prepared material omits a managed delivery"));
            }
        }
        for path in layout_paths(node, &record) {
            if !packet
                .required_scopes
                .contains(&format!("{path}#visual_pending"))
            {
                return Err(invalid(
                    "Layout delivery must start with explicit pending visual coverage",
                ));
            }
        }
        if let Some(current) = &record.current_packet {
            if current.packet_id == packet.packet_id {
                if current.binding != packet.binding
                    || current.manifest != packet.manifest
                    || current.state_sha256 != packet.state_sha256
                    || current.context_epoch != packet.context_epoch
                    || current.machine_checks != packet.machine_checks
                {
                    return Err(conflict(
                        "Review packet identity cannot be reused for changed material",
                    ));
                }
                return Ok(record);
            }
        }
        if record
            .rounds
            .iter()
            .any(|round| round.packet.packet_id == packet.packet_id)
            || record
                .superseded_packets
                .iter()
                .any(|old| old.packet_id == packet.packet_id)
        {
            return Err(conflict(
                "A previous review packet cannot replace newer material",
            ));
        }
        if record.rounds.len() >= record.max_rounds as usize {
            return Err(conflict("Review round limit reached; retain unresolved findings and report the blocked delivery"));
        }
        archive_current_packet(&mut record)?;
        record.current_packet = Some(packet.clone());
        review_save(&tx, &record)?;
        let previous: String = tx
            .query_row(
                "SELECT snapshot_json FROM wc_host_goal_attempts WHERE attempt_id=?1",
                [&binding.attempt_id],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        let mut attempt: HostGoalAttempt = decode(previous)?;
        attempt.checks = packet.machine_checks.clone();
        tx.execute(
            "UPDATE wc_host_goal_attempts SET snapshot_json=?2 WHERE attempt_id=?1",
            params![binding.attempt_id, encode(&attempt)?],
        )
        .map_err(unavailable)?;
        let _ = now; // Packet creation time is part of the immutable material identity.
        tx.commit().map_err(unavailable)?;
        Ok(record)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_host_goal_review_delivery(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        packet_id: &str,
        context_epoch: Option<&str>,
        materials: &[HostGoalReviewMaterial],
        required_scopes: &[String],
        delivered_scopes: &[String],
        _now: i64,
    ) -> Result<HostGoalReviewRecord, GoalStoreError> {
        validate_scopes(required_scopes)?;
        validate_scopes(delivered_scopes)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        review_binding(&tx, owner, binding, context_epoch)?;
        let mut record = review_read(&tx, &binding.goal_id, &binding.node_id)?
            .ok_or_else(|| conflict("Prepare review material before image delivery"))?;
        let packet = record
            .current_packet
            .as_mut()
            .filter(|packet| {
                packet.packet_id == packet_id
                    && packet.binding == *binding
                    && packet.context_epoch.as_deref() == context_epoch
            })
            .ok_or_else(|| conflict("Image delivery targets stale review material"))?;
        if record
            .rounds
            .iter()
            .any(|round| round.packet.packet_id == packet_id)
        {
            return Err(conflict(
                "Submitted material is immutable; prepare a new packet",
            ));
        }
        for material in materials {
            validate_material(material, &packet.manifest, packet.state_sha256.as_deref())?;
            if material.kind == "image" && context_epoch.is_none() {
                return Err(conflict(
                    "Native image delivery requires an identified model conversation",
                ));
            }
            if material.kind == "image"
                && packet.materials.iter().any(|old| {
                    old.kind == "image"
                        && old.path == material.path
                        && old.render_dependencies_sha256 != material.render_dependencies_sha256
                })
            {
                return Err(conflict("Rendering dependencies changed between page deliveries; prepare a fresh packet and return all affected pages"));
            }
            if let Some(old) = packet.materials.iter().find(|old| old.id == material.id) {
                if old != material {
                    return Err(conflict(
                        "Rendered page or dependency changed; prepare fresh review material",
                    ));
                }
            } else {
                packet.materials.push(material.clone());
            }
            if material.kind == "image" {
                packet
                    .required_scopes
                    .retain(|scope| scope != &format!("{}#visual_pending", material.path));
            }
        }
        for scope in required_scopes {
            if !packet.required_scopes.contains(scope) {
                packet.required_scopes.push(scope.clone());
            }
        }
        for scope in delivered_scopes {
            if !packet
                .materials
                .iter()
                .any(|material| material.id == *scope)
            {
                return Err(invalid(
                    "Delivery receipt names material that was never registered",
                ));
            }
            if !packet.delivered_scopes.contains(scope) {
                packet.delivered_scopes.push(scope.clone());
            }
        }
        validate_scopes(&packet.required_scopes)?;
        validate_scopes(&packet.delivered_scopes)?;
        if packet.materials.len() > 512 {
            return Err(invalid("Review material exceeds 512 pages or scopes"));
        }
        review_save(&tx, &record)?;
        tx.commit().map_err(unavailable)?;
        Ok(record)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn submit_host_goal_review(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        packet_id: &str,
        context_epoch: Option<&str>,
        manifest: &[GoalArtifactSnapshot],
        evidence: &[HostGoalCheckEvidence],
        now: i64,
    ) -> Result<HostGoalReviewRecord, GoalStoreError> {
        validate_manifest(manifest)?;
        if evidence.is_empty() || evidence.len() > 64 {
            return Err(invalid(
                "Review requires 1..64 source-backed model verdicts",
            ));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let goal = review_binding(&tx, owner, binding, context_epoch)?;
        let mut record = review_read(&tx, &binding.goal_id, &binding.node_id)?
            .ok_or_else(|| conflict("Review was never prepared"))?;
        let packet = record
            .current_packet
            .as_ref()
            .filter(|packet| {
                packet.packet_id == packet_id
                    && packet.binding == *binding
                    && packet.context_epoch.as_deref() == context_epoch
            })
            .ok_or_else(|| conflict("Model verdict refers to stale or unknown material"))?;
        if packet.manifest != manifest {
            return Err(conflict(
                "Actual artifacts changed since this review packet was prepared",
            ));
        }
        verify_state_material(&goal, packet)?;
        if let Some(round) = record
            .rounds
            .iter()
            .find(|round| round.packet.packet_id == packet_id)
        {
            if round.evidence == evidence {
                return Ok(record);
            }
            return Err(conflict("A submitted model verdict is immutable"));
        }
        if record.rounds.len() >= record.max_rounds as usize {
            return Err(conflict(
                "Review round limit reached; unresolved findings remain blocked",
            ));
        }
        if packet.required_scopes.is_empty()
            || packet.required_scopes.iter().any(|scope| {
                scope.ends_with("#visual_pending") || !packet.delivered_scopes.contains(scope)
            })
        {
            return Err(conflict(
                "Required review material has not all reached this model conversation",
            ));
        }
        let plan = current_plan(&tx, &goal)?;
        let node = plan
            .plan
            .nodes
            .iter()
            .find(|node| node.id == binding.node_id)
            .ok_or_else(|| invalid("Unknown review node"))?;
        if host_goal_review_policy(node).visual || !record.managed_artifacts.is_empty() {
            let paths = layout_paths(node, &record);
            if paths.is_empty() {
                return Err(conflict(
                    "Layout delivery requires an explicit managed_artifacts list before approval",
                ));
            }
            for path in paths {
                if !packet
                    .materials
                    .iter()
                    .any(|item| item.path == path && item.kind == "image")
                {
                    return Err(conflict(
                        "A layout delivery has no actual source-bound images",
                    ));
                }
            }
        }
        let mut reviewed = BTreeSet::new();
        let mut criterion_ids = BTreeSet::new();
        for item in evidence {
            if !criterion_ids.insert(&item.criterion_id) {
                return Err(invalid("Duplicate model review criterion"));
            }
            if item.source != HostGoalEvidenceSource::ModelSelfCheck
                || item.review_packet_id.as_deref() != Some(packet_id)
                || item.source_refs.is_empty()
                || item.evidence.trim().chars().count() < 40
            {
                return Err(invalid("Model review requires original packet identity, sources and a concrete explanation of scope and findings"));
            }
            bounded(&item.evidence, 8192, "review explanation")?;
            if item.source_refs.len() > 64 {
                return Err(invalid("Review source references exceed 64 entries"));
            }
            for source in &item.source_refs {
                bounded(source, 4096, "review source reference")?;
            }
            let distinct = item
                .evidence
                .chars()
                .filter(|ch| ch.is_alphanumeric())
                .collect::<BTreeSet<_>>();
            if distinct.len() < 10
                || packet.materials.iter().any(|material| {
                    material
                        .content
                        .as_deref()
                        .is_some_and(|content| content.trim() == item.evidence.trim())
                })
            {
                return Err(invalid("Review explanation must contain a new concrete assessment, not repeated boilerplate or copied prepared material"));
            }
            let explains_location = packet.manifest.iter().any(|artifact| {
                item.evidence.contains(&artifact.path)
                    || item
                        .findings
                        .iter()
                        .any(|finding| finding.location.contains(&artifact.path))
            }) || item
                .reviewed_scopes
                .iter()
                .any(|scope| item.evidence.contains(scope));
            if !explains_location {
                return Err(invalid("Review explanation or findings must name an actual source file or reviewed scope"));
            }
            if item.passed
                && record
                    .rounds
                    .iter()
                    .filter(|round| {
                        round.packet.manifest != packet.manifest
                            || round.packet.state_sha256 != packet.state_sha256
                    })
                    .any(|round| {
                        round.evidence.iter().any(|old| {
                            old.passed
                                && old.reviewed_scopes == item.reviewed_scopes
                                && old.evidence.trim() == item.evidence.trim()
                        })
                    })
            {
                return Err(invalid("Changed artifacts require a fresh assessment rather than copying an earlier version's passing explanation"));
            }
            if item.findings.len() > 64
                || (item.passed && !item.findings.is_empty())
                || (!item.passed && item.findings.is_empty())
            {
                return Err(invalid("Failed reviews require specific findings; passing reviews cannot retain unresolved findings"));
            }
            for finding in &item.findings {
                bounded(&finding.location, 1024, "finding location")?;
                bounded(&finding.impact, 2048, "finding impact")?;
                bounded(&finding.required_change, 2048, "finding required change")?;
            }
            validate_scopes(&item.reviewed_scopes)?;
            if item.reviewed_scopes.is_empty()
                || item
                    .reviewed_scopes
                    .iter()
                    .any(|scope| !packet.delivered_scopes.contains(scope))
            {
                return Err(invalid(
                    "Model verdict scope must identify material delivered in this conversation",
                ));
            }
            if item.artifacts != packet.manifest {
                return Err(conflict(
                    "Model verdict must retain the complete original artifact manifest",
                ));
            }
            reviewed.extend(item.reviewed_scopes.iter().cloned());
        }
        if packet
            .required_scopes
            .iter()
            .any(|scope| !reviewed.contains(scope))
        {
            return Err(conflict("Model review has not covered the whole delivery"));
        }
        let passed = evidence.iter().all(|item| item.passed);
        record.rounds.push(HostGoalReviewRound {
            round: record.rounds.len() as u8 + 1,
            packet: packet.clone(),
            evidence: evidence.to_vec(),
            passed,
            submitted_at: now,
        });
        review_save(&tx, &record)?;
        if !passed {
            let failure = evidence
                .iter()
                .filter(|item| !item.passed)
                .map(|item| format!("{}: {}", item.criterion_id, item.evidence))
                .collect::<Vec<_>>()
                .join("\n");
            super::repair::fail(&tx, &goal, binding, &failure, now)?;
            if record.rounds.len() >= record.max_rounds as usize {
                super::repair::exhaust(&tx, &goal.goal_id)?;
            }
        }
        tx.commit().map_err(unavailable)?;
        Ok(record)
    }
}
