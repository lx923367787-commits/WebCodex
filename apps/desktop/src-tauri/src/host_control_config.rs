//! Candidate configuration for the existing, exact `chat-agents` provider.
//! The frozen `chat_agents_v1` field requires the D/E lifecycle implementation:
//! writing these passive paths neither commits a provider nor activates dispatch.
use crate::error::{DesktopError, DesktopResult};
use std::io::Read;
use std::path::{Path, PathBuf};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

const MAX_CONFIG_BYTES: usize = 256 * 1024;
const PROVIDER_ID: &str = "chat-agents";
const CONTROL_FLAG: &str = "--control-config";
const STATE_FLAG: &str = "--desktop-state";

/// Local serialization of the frozen contract, not an activation receipt.
#[derive(Clone, Copy)]
enum RuntimeLifecycle {
    ChatAgentsV1,
}

impl RuntimeLifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::ChatAgentsV1 => "chat_agents_v1",
        }
    }
}

pub(crate) struct ChatAgentConfigChange {
    path: PathBuf,
    before: Table,
    installed: Table,
    committed: bool,
}

impl ChatAgentConfigChange {
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }

    fn rollback(&self) -> DesktopResult<()> {
        let current = read_config(&self.path)?;
        let restored = rollback_content(&current, &self.before, &self.installed)?;
        // Match the existing Desktop configuration transaction convention.
        // The owning runtime must serialize its producers; this comparison is
        // not an OS-level CAS against uncooperative external file writers.
        if restored != current && read_config(&self.path)? == current {
            crate::state::write_atomic_file(&self.path, restored.as_bytes())
                .map_err(|_| config_error("Could not restore the chat-agents configuration"))?;
        }
        Ok(())
    }
}

impl Drop for ChatAgentConfigChange {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.rollback();
        }
    }
}

/// Change only an existing exact provider. `None` means either absent or already
/// configured; use `validate_content` to distinguish candidate readiness.
pub(crate) fn configure(
    path: &Path,
    control_config: &Path,
    desktop_state: &Path,
) -> DesktopResult<Option<ChatAgentConfigChange>> {
    let original = read_config(path)?;
    let Some((updated, before, installed)) =
        candidate_content(&original, control_config, desktop_state)?
    else {
        return Ok(None);
    };
    if read_config(path)? != original {
        return Err(config_error(
            "Runner configuration changed while preparing chat-agents",
        ));
    }
    // Own recovery before publication, including an atomic writer reporting an
    // error after replacing the destination (for example directory sync).
    let change = ChatAgentConfigChange {
        path: path.to_owned(),
        before,
        installed,
        committed: false,
    };
    crate::state::write_atomic_file(path, updated.as_bytes())
        .map_err(|_| config_error("Could not save the chat-agents configuration"))?;
    Ok(Some(change))
}

/// Validate passive candidate configuration without opening either supplied
/// path, loading credentials, or checking whether D has activated a provider.
pub(crate) fn validate_content(
    content: &str,
    control_config: &Path,
    desktop_state: &Path,
) -> DesktopResult<bool> {
    let control = path_argument(control_config)?;
    let state = path_argument(desktop_state)?;
    let doc = parse_content(content)?;
    Ok(inspect(&doc, control, state)?.is_some_and(|row| row.ready()))
}

struct RowState {
    index: usize,
    control_present: bool,
    state_present: bool,
    lifecycle_present: bool,
}

impl RowState {
    fn ready(&self) -> bool {
        self.control_present && self.state_present && self.lifecycle_present
    }
}

fn parse_content(content: &str) -> DesktopResult<DocumentMut> {
    if content.len() > MAX_CONFIG_BYTES {
        return Err(config_error("Runner configuration exceeds its size limit"));
    }
    content
        .parse()
        .map_err(|_| config_error("Runner configuration is invalid TOML"))
}

fn path_argument(path: &Path) -> DesktopResult<&str> {
    let text = path
        .to_str()
        .filter(|text| {
            path.is_absolute() && text.len() <= 32_768 && !text.chars().any(char::is_control)
        })
        .ok_or_else(|| config_error("Host control paths must be absolute UTF-8 paths"))?;
    Ok(text)
}

fn providers(doc: &DocumentMut) -> DesktopResult<Option<&ArrayOfTables>> {
    let Some(plugins) = doc.get("plugins") else {
        return Ok(None);
    };
    let plugins = plugins
        .as_table()
        .ok_or_else(|| config_error("Runner plugins configuration is not a table"))?;
    let Some(providers) = plugins.get("providers") else {
        return Ok(None);
    };
    providers
        .as_array_of_tables()
        .map(Some)
        .ok_or_else(|| config_error("Runner plugin providers are not a table array"))
}

fn row_index(providers: &ArrayOfTables) -> DesktopResult<Option<usize>> {
    let mut matches = providers
        .iter()
        .enumerate()
        .filter(|(_, row)| row.get("id").and_then(Item::as_str) == Some(PROVIDER_ID));
    let first = matches.next().map(|(index, _)| index);
    if matches.next().is_some() {
        return Err(config_error(
            "Runner contains duplicate chat-agents providers",
        ));
    }
    Ok(first)
}

fn inspect(doc: &DocumentMut, control: &str, state: &str) -> DesktopResult<Option<RowState>> {
    let Some(providers) = providers(doc)? else {
        return Ok(None);
    };
    let Some(index) = row_index(providers)? else {
        return Ok(None);
    };
    let row = providers.get(index).expect("located provider");
    let lifecycle_present = match row.get("runtime_lifecycle") {
        None => false,
        Some(item) if item.as_str() == Some(RuntimeLifecycle::ChatAgentsV1.as_str()) => true,
        Some(_) => return Err(config_error("Unsupported chat-agents runtime lifecycle")),
    };
    let mut result = RowState {
        index,
        control_present: false,
        state_present: false,
        lifecycle_present,
    };
    let Some(args) = row.get("args") else {
        return Ok(Some(result));
    };
    let args = args
        .as_array()
        .ok_or_else(|| config_error("chat-agents arguments are not an array"))?;
    let args = args
        .iter()
        .map(|arg| {
            arg.as_str()
                .ok_or_else(|| config_error("chat-agents arguments must be strings"))
        })
        .collect::<DesktopResult<Vec<_>>>()?;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index];
        if arg == "--"
            || arg.starts_with("--control-config=")
            || arg.starts_with("--desktop-state=")
        {
            return Err(config_error(
                "Unsupported chat-agents control argument syntax",
            ));
        }
        let expected = match arg {
            CONTROL_FLAG => Some((control, &mut result.control_present)),
            STATE_FLAG => Some((state, &mut result.state_present)),
            _ => None,
        };
        if let Some((expected, present)) = expected {
            if *present || args.get(index + 1).copied() != Some(expected) {
                return Err(config_error(
                    "Conflicting or repeated chat-agents control arguments",
                ));
            }
            *present = true;
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok(Some(result))
}

fn candidate_content(
    original: &str,
    control_config: &Path,
    desktop_state: &Path,
) -> DesktopResult<Option<(String, Table, Table)>> {
    let control = path_argument(control_config)?;
    let state = path_argument(desktop_state)?;
    let mut doc = parse_content(original)?;
    let Some(observed) = inspect(&doc, control, state)? else {
        return Ok(None);
    };
    if observed.ready() {
        return Ok(None);
    }
    let row = doc["plugins"]["providers"]
        .as_array_of_tables_mut()
        .expect("inspected provider array")
        .get_mut(observed.index)
        .expect("inspected provider index");
    let before = row.clone();
    if !observed.control_present || !observed.state_present {
        if row.get("args").is_none() {
            row["args"] = value(Array::new());
        }
        let args = row["args"].as_array_mut().expect("inspected arguments");
        if !observed.control_present {
            args.push(CONTROL_FLAG);
            args.push(control);
        }
        if !observed.state_present {
            args.push(STATE_FLAG);
            args.push(state);
        }
    }
    if !observed.lifecycle_present {
        row["runtime_lifecycle"] = value(RuntimeLifecycle::ChatAgentsV1.as_str());
    }
    let installed = row.clone();
    let updated = doc.to_string();
    if updated.len() > MAX_CONFIG_BYTES {
        return Err(config_error(
            "Updated Runner configuration exceeds its size limit",
        ));
    }
    Ok(Some((updated, before, installed)))
}

fn rollback_content(current: &str, before: &Table, installed: &Table) -> DesktopResult<String> {
    let mut doc = parse_content(current)?;
    let Some(rows) = providers(&doc)? else {
        return Ok(current.to_owned());
    };
    let Some(index) = row_index(rows)? else {
        return Ok(current.to_owned());
    };
    if rows.get(index).expect("located provider").to_string() != installed.to_string() {
        return Ok(current.to_owned());
    }
    let row = doc["plugins"]["providers"]
        .as_array_of_tables_mut()
        .expect("inspected provider array")
        .get_mut(index)
        .expect("inspected provider index");
    // Keep the current table's document position and header formatting: an
    // operator may have inserted an unrelated table before this provider.
    for key in ["args", "runtime_lifecycle"] {
        match before.get(key) {
            Some(previous) => row[key] = previous.clone(),
            None => {
                row.remove(key);
            }
        }
    }
    Ok(doc.to_string())
}

fn read_config(path: &Path) -> DesktopResult<String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| config_error("Could not inspect Runner configuration"))?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(config_error(
            "Runner configuration must be a bounded regular file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(config_error(
                "Runner configuration cannot be a reparse point",
            ));
        }
    }
    let file = std::fs::File::open(path)
        .map_err(|_| config_error("Could not open Runner configuration"))?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| config_error("Could not read Runner configuration"))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(config_error("Runner configuration exceeds its size limit"));
    }
    String::from_utf8(bytes).map_err(|_| config_error("Runner configuration is not UTF-8"))
}

fn config_error(message: &str) -> DesktopError {
    DesktopError::new(
        "chat_agents_config_invalid",
        message,
        "Check the existing chat-agents Runner configuration, then retry.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "# retained\n[policy]\nallow_cwd_anywhere = false\n[[plugins.providers]]\nid = 'other'\ncommand = 'other.exe'\nargs = ['--control-config=untouched']\n[[plugins.providers]]\nid = 'chat-agents'\ncommand = 'python-custom'\nargs = ['chosen.py', '--runtime', 'runtime-value']\ncwd = 'chosen-cwd'\n";

    fn paths() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir();
        (
            root.join("control-fixture.json"),
            root.join("desktop-fixture.json"),
        )
    }

    fn candidate(source: &str) -> DesktopResult<Option<(String, Table, Table)>> {
        let (control, state) = paths();
        candidate_content(source, &control, &state)
    }

    fn ready(source: &str) -> DesktopResult<bool> {
        let (control, state) = paths();
        validate_content(source, &control, &state)
    }

    #[test]
    fn absent_provider_is_not_created_or_confused_with_a_prefix() {
        for source in [
            "",
            "[plugins]\n",
            "[[plugins.providers]]\nid='chat-agents-copy'\nruntime_lifecycle='unknown'\n",
        ] {
            assert!(candidate(source).unwrap().is_none());
            assert!(!ready(source).unwrap());
        }
    }

    #[test]
    fn configures_only_exact_row_and_is_idempotent() {
        let (updated, before, mut installed) = candidate(ORIGINAL).unwrap().unwrap();
        assert!(ready(&updated).unwrap());
        assert!(candidate(&updated).unwrap().is_none());
        let original = parse_content(ORIGINAL).unwrap();
        let after = parse_content(&updated).unwrap();
        assert_eq!(original["policy"].to_string(), after["policy"].to_string());
        assert_eq!(
            providers(&original)
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap()
                .to_string(),
            providers(&after)
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap()
                .to_string()
        );
        assert!(updated.starts_with("# retained\n"));
        assert_eq!(installed["command"].as_str(), Some("python-custom"));
        assert_eq!(installed["cwd"].as_str(), Some("chosen-cwd"));
        installed.remove("runtime_lifecycle");
        installed["args"] = before["args"].clone();
        assert_eq!(installed.to_string(), before.to_string());
    }

    #[test]
    fn adds_only_missing_argument_pairs_and_accepts_absent_args() {
        let (updated, _, _) = candidate(ORIGINAL).unwrap().unwrap();
        let mut doc = parse_content(&updated).unwrap();
        let rows = doc["plugins"]["providers"]
            .as_array_of_tables_mut()
            .unwrap();
        let row = rows.get_mut(1).unwrap();
        let args = row["args"].as_array_mut().unwrap();
        args.remove(args.len() - 1);
        args.remove(args.len() - 1);
        assert!(!ready(&doc.to_string()).unwrap());
        assert_eq!(candidate(&doc.to_string()).unwrap().unwrap().0, updated);
        let no_args = "[[plugins.providers]]\nid='chat-agents'\ncommand='service.exe'\n";
        assert!(ready(&candidate(no_args).unwrap().unwrap().0).unwrap());
    }

    #[test]
    fn rejects_duplicate_rows_and_unknown_lifecycle_without_rewriting() {
        let duplicate = format!("{ORIGINAL}\n[[plugins.providers]]\nid='chat-agents'\n");
        assert!(candidate(&duplicate).is_err());
        for lifecycle in ["'future'", "42", "false", "{}"] {
            let source = format!("{ORIGINAL}runtime_lifecycle={lifecycle}\n");
            assert!(candidate(&source).is_err());
            assert!(ready(&source).is_err());
        }
    }

    #[test]
    fn rejects_conflicting_duplicate_and_unsupported_argument_forms() {
        let (updated, _, _) = candidate(ORIGINAL).unwrap().unwrap();
        for flag in [CONTROL_FLAG, STATE_FLAG] {
            for mode in ["duplicate", "equals", "wrong", "missing"] {
                let mut doc = parse_content(&updated).unwrap();
                let row = doc["plugins"]["providers"]
                    .as_array_of_tables_mut()
                    .unwrap()
                    .get_mut(1)
                    .unwrap();
                let args = row["args"].as_array_mut().unwrap();
                let index = args
                    .iter()
                    .position(|arg| arg.as_str() == Some(flag))
                    .unwrap();
                let expected = args.get(index + 1).unwrap().as_str().unwrap().to_owned();
                match mode {
                    "duplicate" => {
                        args.push(flag);
                        args.push(expected);
                    }
                    "equals" => {
                        args.replace(index, format!("{flag}={expected}"));
                        args.remove(index + 1);
                    }
                    "wrong" => {
                        args.replace(index + 1, "different-path");
                    }
                    "missing" => {
                        args.remove(index + 1);
                    }
                    _ => unreachable!(),
                }
                assert!(candidate(&doc.to_string()).is_err(), "{flag}: {mode}");
            }
        }
        for args in ["[42]", "'not-array'", "['chosen.py', '--']"] {
            let source = format!("[[plugins.providers]]\nid='chat-agents'\nargs={args}\n");
            assert!(candidate(&source).is_err());
        }
    }

    #[test]
    fn rollback_preserves_other_rows_and_leaves_changed_or_removed_target() {
        let (updated, before, installed) = candidate(ORIGINAL).unwrap().unwrap();
        assert_eq!(
            rollback_content(&updated, &before, &installed).unwrap(),
            ORIGINAL
        );
        let unrelated = updated.replace("other.exe", "operator-new.exe");
        let restored = rollback_content(&unrelated, &before, &installed).unwrap();
        assert_eq!(restored, ORIGINAL.replace("other.exe", "operator-new.exe"));
        let inserted_table = format!("[added]\nvalue=1\n{updated}");
        assert_eq!(
            rollback_content(&inserted_table, &before, &installed).unwrap(),
            format!("[added]\nvalue=1\n{ORIGINAL}")
        );
        for changed in [
            updated.replace("python-custom", "operator-python"),
            updated.replace("id = 'chat-agents'", "id = 'renamed-provider'"),
        ] {
            assert_eq!(
                rollback_content(&changed, &before, &installed).unwrap(),
                changed
            );
        }
        assert!(rollback_content(
            &format!("{updated}\n[[plugins.providers]]\nid='chat-agents'\n"),
            &before,
            &installed
        )
        .is_err());
    }

    #[test]
    fn rejects_relative_paths_and_malformed_or_oversized_config() {
        let (control, state) = paths();
        assert!(validate_content(ORIGINAL, Path::new("relative.json"), &state).is_err());
        assert!(validate_content(ORIGINAL, &control, Path::new("relative.json")).is_err());
        for source in [
            "[broken".to_owned(),
            "plugins=3".to_owned(),
            "[plugins]\nproviders=3".to_owned(),
            " ".repeat(MAX_CONFIG_BYTES + 1),
        ] {
            assert!(ready(&source).is_err());
        }
    }
}
