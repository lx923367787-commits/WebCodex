//! Immutable, local, reviewable plan artifacts. Publishing files is separate
//! from the database receipt: callers may mark a plan materialized only after
//! this module has verified both artifacts, including on an uncertain retry.
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_PLAN_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PlanArtifacts {
    pub json_path: String,
    pub json_sha256: String,
    pub markdown_path: String,
    pub markdown_sha256: String,
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn plain(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

fn directory(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("Cannot create local plan directory: {error}")),
    }
    verify_directory(path)
}

fn verify_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || !plain(&metadata) {
        return Err(
            "Plan directories must be ordinary directories, not links or reparse points".into(),
        );
    }
    Ok(())
}

fn read_plain(path: &Path) -> Result<Vec<u8>, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Plan artifact is missing".to_string())?;
    if !metadata.is_file() || !plain(&metadata) || metadata.len() > MAX_PLAN_FILE_BYTES {
        return Err("Plan artifact must be a bounded ordinary file".into());
    }
    fs::read(path).map_err(|e| e.to_string())
}

fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

fn bullet_values(output: &mut String, values: Option<&Value>) {
    if let Some(values) = values.and_then(Value::as_array) {
        for value in values {
            if let Some(value) = value.as_str() {
                output.push_str(&format!("- {value}\n"));
            } else {
                output.push_str(&format!("- {}\n", value));
            }
        }
    }
    output.push('\n');
}

pub(crate) fn markdown(plan: &Value, goal_id: &str, version: u64, plan_hash: &str) -> String {
    let mut text = format!("# 目标执行计划\n\n目标：{goal_id}\n\n计划版本：{version}\n\n计划哈希：{plan_hash}\n\n**请先阅读本计划，再在 ChatGPT 对话中明确通知开始执行此版本。保存计划不代表已经确认执行。**\n\n## 最终目标\n\n{}\n\n## 约束\n\n", string(&plan["objective"]));
    bullet_values(&mut text, plan.get("constraints"));
    text.push_str("## 最终交付\n\n");
    bullet_values(&mut text, plan.get("deliverables"));
    text.push_str("## 验收输入与产物指纹\n\n命令验收必须明确列出会影响检查的项目内文件；文件指纹无法读取时不能通过验收或复用旧命令结果。\n\n");
    bullet_values(&mut text, plan.get("verification_paths"));
    text.push_str("## 最终验收目标与方式\n\n");
    if let Some(criteria) = plan["final_criteria"].as_array() {
        for criterion in criteria {
            text.push_str(&format!(
                "### {} — {}\n\n验收方式：{}\n\n",
                string(&criterion["id"]),
                string(&criterion["description"]),
                string(&criterion["kind"])
            ));
            // The fenced block is generated JSON, not executable instructions.
            text.push_str(&format!(
                "````json\n{}\n````\n\n",
                serde_json::to_string_pretty(criterion).unwrap_or_default()
            ));
        }
    }
    text.push_str("## State 管理与恢复规则\n\n");
    text.push_str(&format!(
        "````json\n{}\n````\n\n",
        serde_json::to_string_pretty(&plan["state_management"]).unwrap_or_default()
    ));
    text.push_str("## 逐步执行与节点验收\n\n");
    if let Some(nodes) = plan["nodes"].as_array() {
        for (index, node) in nodes.iter().enumerate() {
            text.push_str(&format!(
                "### {}. {} ({})\n\n",
                index + 1,
                string(&node["title"]),
                string(&node["id"])
            ));
            text.push_str(&format!(
                "````json\n{}\n````\n\n",
                serde_json::to_string_pretty(node).unwrap_or_default()
            ));
        }
    }
    text.push_str("## 执行及交接\n\n每个节点执行前读取当前 State，阶段结果和问题及时保存检查点；节点验收通过后再推进。遇到结果未知的操作先核对原 Job，不能盲目重做。新对话输入“继续本地当前任务”读取已保存的计划和当前进度。计划修改后需要重新确认新版本。\n");
    text
}

fn plan_dir(root: &Path, goal_id: &str, version: u64, create: bool) -> Result<PathBuf, String> {
    if goal_id.is_empty()
        || goal_id.len() > 128
        || !goal_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        || version == 0
    {
        return Err("Invalid local plan identity".into());
    }
    let check = if create { directory } else { verify_directory };
    check(root)?;
    let goal = root.join(goal_id);
    check(&goal)?;
    let plans = goal.join("plans");
    check(&plans)?;
    Ok(plans.join(format!("v{version}")))
}

pub(crate) fn materialize(
    root: &Path,
    goal_id: &str,
    version: u64,
    plan_hash: &str,
    plan: &Value,
) -> Result<PlanArtifacts, String> {
    let destination = plan_dir(root, goal_id, version, true)?;
    let mut json = serde_json::to_vec_pretty(plan).map_err(|e| e.to_string())?;
    json.push(b'\n');
    let markdown = markdown(plan, goal_id, version, plan_hash).into_bytes();
    if json.len() as u64 > MAX_PLAN_FILE_BYTES || markdown.len() as u64 > MAX_PLAN_FILE_BYTES {
        return Err("Plan artifacts exceed the local file size bound".into());
    }
    if !destination.try_exists().map_err(|e| e.to_string())? {
        let staging = tempfile::Builder::new()
            .prefix(".publishing-")
            .tempdir_in(destination.parent().unwrap())
            .map_err(|e| e.to_string())?;
        for (name, bytes) in [
            ("plan.json", json.as_slice()),
            ("plan.md", markdown.as_slice()),
        ] {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(staging.path().join(name))
                .map_err(|e| e.to_string())?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
        }
        if let Err(error) = fs::rename(staging.path(), &destination) {
            if !destination.try_exists().map_err(|e| e.to_string())? {
                return Err(format!("Cannot publish local plan: {error}"));
            }
        }
    }
    let metadata = fs::symlink_metadata(&destination).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || !plain(&metadata) {
        return Err("Plan version directory was replaced".into());
    }
    let json_path = destination.join("plan.json");
    let markdown_path = destination.join("plan.md");
    if read_plain(&json_path)? != json || read_plain(&markdown_path)? != markdown {
        return Err("Local plan artifacts do not match the immutable saved version; execution remains blocked".into());
    }
    Ok(PlanArtifacts {
        json_path: json_path.to_string_lossy().into_owned(),
        json_sha256: sha256(&json),
        markdown_path: markdown_path.to_string_lossy().into_owned(),
        markdown_sha256: sha256(&markdown),
    })
}

pub(crate) fn verify(
    root: &Path,
    goal_id: &str,
    version: u64,
    artifacts: &PlanArtifacts,
) -> Result<(), String> {
    let destination = plan_dir(root, goal_id, version, false)?;
    let metadata = fs::symlink_metadata(&destination)
        .map_err(|_| "Plan version directory is missing".to_string())?;
    if !metadata.is_dir() || !plain(&metadata) {
        return Err("Plan version directory was replaced".into());
    }
    for (name, expected_path, expected_hash) in [
        ("plan.json", &artifacts.json_path, &artifacts.json_sha256),
        (
            "plan.md",
            &artifacts.markdown_path,
            &artifacts.markdown_sha256,
        ),
    ] {
        let path = destination.join(name);
        if path != Path::new(expected_path) || sha256(&read_plain(&path)?) != *expected_hash {
            return Err("Local plan is missing, changed, or belongs to a different version; save and confirm the correct plan before execution".into());
        }
    }
    Ok(())
}
