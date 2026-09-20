//! Optional installer payload. No host Python, OpenAI credential, or project script is used.
use crate::error::{DesktopError, DesktopResult};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const TOKEN_FILE_KEY: &str = "WEBCODEX_TUNNEL_MCP_TOKEN_FILE";
const TRUST_KEY: &str = "WEBCODEX_TRUSTED_MCP_FILE_API_TOKEN_IDS";

pub(crate) struct BundledTools {
    python: PathBuf,
    plugin: PathBuf,
    font: PathBuf,
}

/// Roll back only the provider edit owned by this setup attempt. Project
/// activation can also edit policy, so unrelated changes are preserved.
pub(crate) struct RunnerConfigChange {
    path: PathBuf,
    original: String,
    installed: String,
    committed: bool,
}

impl RunnerConfigChange {
    pub fn commit(mut self) {
        self.committed = true;
    }

    fn rollback(&self) -> DesktopResult<()> {
        let current = read_config(&self.path)?;
        let restored = rollback_runner_content(&self.original, &self.installed, &current)?;
        if restored != current && read_config(&self.path)? == current {
            crate::state::write_atomic_file(&self.path, restored.as_bytes()).map_err(|_| {
                config_error("Could not restore the previous bundled-tools project")
            })?;
        }
        Ok(())
    }
}

impl Drop for RunnerConfigChange {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.rollback();
        }
    }
}

fn rollback_runner_content(
    original: &str,
    installed: &str,
    current: &str,
) -> DesktopResult<String> {
    if current == installed {
        return Ok(original.to_string());
    }
    let parse = |s: &str| {
        s.parse::<DocumentMut>()
            .map_err(|_| config_error("Runner configuration changed during setup recovery"))
    };
    let before = parse(original)?;
    let written = parse(installed)?;
    let mut now = parse(current)?;
    let provider = |doc: &DocumentMut| {
        doc.get("plugins")
            .and_then(|p| p.get("providers"))
            .and_then(Item::as_array_of_tables)
            .and_then(|providers| {
                providers
                    .iter()
                    .find(|p| p.get("id").and_then(Item::as_str) == Some("mycodex-tools"))
            })
            .cloned()
    };
    let expected = provider(&written);
    if provider(&now).map(|p| p.to_string()) != expected.map(|p| p.to_string()) {
        return Ok(current.to_string()); // An operator changed this provider: do not clobber it.
    }
    if let Some(providers) = now
        .get_mut("plugins")
        .and_then(|p| p.get_mut("providers"))
        .and_then(Item::as_array_of_tables_mut)
    {
        let index = providers
            .iter()
            .position(|p| p.get("id").and_then(Item::as_str) == Some("mycodex-tools"));
        if let Some(index) = index {
            match provider(&before) {
                Some(previous) => *providers.get_mut(index).expect("located provider") = previous,
                None => {
                    providers.remove(index);
                }
            }
        }
    }
    Ok(now.to_string())
}

// Intentionally no Debug: the CLI response contains a one-time local credential.
#[derive(Deserialize)]
pub(crate) struct BridgeToken {
    pub token: String,
    pub token_id: String,
}

pub(crate) struct BootstrapCredentialFile(PathBuf);

impl BootstrapCredentialFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for BootstrapCredentialFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The enrolled runtime token deliberately lacks account:manage. Mint the
/// dedicated PAT with the Desktop-owned Server credential, without expanding
/// the enrollment scopes or ever putting a credential in argv or diagnostics.
pub(crate) fn bootstrap_credential_file(env_file: &Path) -> DesktopResult<BootstrapCredentialFile> {
    use std::io::Write;
    let content = read_config(env_file)?;
    let token = env_value(&content, "WEBCODEX_TOKEN")
        .map(|value| value.trim_matches(['\'', '"']))
        .filter(|value| {
            !value.is_empty() && value.len() <= 8192 && value.bytes().all(|b| b > b' ' && b < 127)
        })
        .ok_or_else(|| config_error("Local Server management credential is unavailable"))?;
    let directory = env_file
        .parent()
        .ok_or_else(|| config_error("Invalid Server configuration path"))?
        .join("file-bridge");
    std::fs::create_dir_all(&directory)
        .map_err(|_| config_error("Could not prepare local credential storage"))?;
    protect_secret_directory(&directory)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| config_error("Local clock is unavailable"))?
        .as_nanos();
    let path = directory.join(format!(
        ".bootstrap-once-{}-{nonce}.token",
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|_| config_error("Could not prepare the local management request"))?;
    // Cleanup ownership begins only after create_new succeeds.
    let credential = BootstrapCredentialFile(path);
    file.write_all(token.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| config_error("Could not prepare the local management request"))?;
    Ok(credential)
}

impl BundledTools {
    pub fn discover(resource_dir: &Path) -> DesktopResult<Option<Self>> {
        let root = resource_dir.join("mycodex-tools");
        if !root.exists() {
            return Ok(None);
        }
        let bundle = Self {
            python: root.join("python").join(if cfg!(windows) {
                "python.exe"
            } else {
                "python"
            }),
            plugin: root.join("plugin.py"),
            font: root.join("fonts").join("ZCOOLXiaoWei-Regular.ttf"),
        };
        for path in [&bundle.python, &bundle.plugin, &bundle.font] {
            regular_file(path, None)?;
        }
        Ok(Some(bundle))
    }

    /// Preserve unrelated settings and an explicitly configured provider of the same ID.
    /// Only the provider pointing at this exact installer payload is Desktop-managed.
    pub fn configure_runner(
        &self,
        path: &Path,
        project: &Path,
    ) -> DesktopResult<Option<RunnerConfigChange>> {
        let original = read_config(path)?;
        let updated = self.runner_content(&original, project)?;
        if updated == original {
            return Ok(None);
        }
        // Do not overwrite a concurrent operator edit between inspection and publication.
        if read_config(path)? != original {
            return Err(config_error(
                "Runner configuration changed while enabling bundled tools",
            ));
        }
        crate::state::write_atomic_file(path, updated.as_bytes())
            .map_err(|_| config_error("Could not save the bundled tools configuration"))?;
        Ok(Some(RunnerConfigChange {
            path: path.to_owned(),
            original,
            installed: updated,
            committed: false,
        }))
    }

    fn runner_content(&self, original: &str, project: &Path) -> DesktopResult<String> {
        if !project.is_absolute() || !project.is_dir() {
            return Err(config_error(
                "The selected bundled-tools project is unavailable",
            ));
        }
        let mut doc = original
            .parse::<DocumentMut>()
            .map_err(|_| config_error("Runner configuration is invalid TOML"))?;
        if doc.get("plugins").is_none() {
            doc["plugins"] = Item::Table(Table::new());
        }
        let plugins = doc["plugins"]
            .as_table_mut()
            .ok_or_else(|| config_error("Runner plugins configuration is not a table"))?;
        if !plugins.contains_key("providers") {
            plugins["providers"] = Item::ArrayOfTables(ArrayOfTables::new());
        }
        let providers = plugins["providers"]
            .as_array_of_tables_mut()
            .ok_or_else(|| config_error("Runner plugin providers are not a table array"))?;
        let existing = providers.iter().position(|provider| {
            provider.get("id").and_then(Item::as_str) == Some("mycodex-tools")
        });
        let python = self.python.to_string_lossy().to_string();
        let plugin = self.plugin.to_string_lossy().to_string();
        let mut args = Array::new();
        args.push(plugin.as_str());
        args.push("--font-path");
        args.push(self.font.to_string_lossy().as_ref());
        if let Some(index) = existing {
            let provider = providers.get_mut(index).expect("existing provider index");
            let own_args = provider
                .get("args")
                .and_then(Item::as_array)
                .and_then(|args| args.get(0))
                .and_then(|arg| arg.as_str())
                == Some(plugin.as_str());
            if provider.get("command").and_then(Item::as_str) != Some(python.as_str()) || !own_args
            {
                return Ok(original.to_string());
            }
            provider["cwd"] = value(project.to_string_lossy().as_ref());
        } else {
            let mut provider = Table::new();
            provider["id"] = value("mycodex-tools");
            provider["name"] = value("MyCodex Office and PDF");
            provider["command"] = value(python);
            provider["args"] = value(args);
            provider["cwd"] = value(project.to_string_lossy().as_ref());
            provider["timeout_secs"] = value(60);
            providers.push(provider);
            if !plugins.contains_key("request_timeout_secs") {
                plugins["request_timeout_secs"] = value(60);
            }
        }
        Ok(doc.to_string())
    }
}

#[cfg(test)]
pub(crate) async fn probe_live_bundle(
    tools: &BundledTools,
    identity: &crate::webcodex::ProjectRuntimeIdentity,
    runner: &str,
    env_file: &Path,
    cancellation: &crate::operation::CancellationContext,
) -> DesktopResult<serde_json::Value> {
    // Only fixture-local credentials are read by this child; none are arguments,
    // emitted in output, or passed through a model. HTTP is loopback with no proxy.
    let script = r#"
import http.client,json,sys
from pathlib import Path
from urllib.parse import urlsplit
c=json.loads(sys.argv[1]); env={}
for line in Path(c['env']).read_text(encoding='utf-8').splitlines():
    if '=' in line and not line.strip().startswith('#'):
        k,v=line.split('=',1); env[k.strip()]=v.strip().strip('\"').strip("'")
token=Path(env['WEBCODEX_TUNNEL_MCP_TOKEN_FILE']).read_text().strip()
assert token.startswith('wc_pat_') and token != env['WEBCODEX_TOKEN']
url=urlsplit(c['url']); session=None; seq=0
def rpc(name,args):
    global seq,session
    seq+=1
    h={'Authorization':'Bearer '+token,'Content-Type':'application/json','Accept':'application/json, text/event-stream','MCP-Protocol-Version':'2025-06-18'}
    if session: h['Mcp-Session-Id']=session
    conn=http.client.HTTPConnection(url.hostname,url.port,timeout=15)
    conn.request('POST','/mcp',json.dumps({'jsonrpc':'2.0','id':seq,'method':name,'params':args}),h)
    res=conn.getresponse(); raw=res.read(2*1024*1024).decode(); session=res.getheader('Mcp-Session-Id') or session
    assert res.status==200, 'MCP HTTP failed'
    body=next((json.loads(x[5:].strip()) for x in raw.splitlines() if x.startswith('data:')),None) if res.getheader('Content-Type','').startswith('text/event-stream') else json.loads(raw)
    conn.close(); assert 'error' not in body, 'MCP RPC failed'; return body['result']
def init():
    return rpc('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'desktop-bundle-acceptance','version':'1'}})
def call(name,args): return rpc('tools/call',{'name':name,'arguments':args})
def output(r):
    assert not r.get('isError'), 'Plugin call failed'
    value=r.get('structuredContent')
    if value is None: value=next(json.loads(x['text']) for x in r.get('content',[]) if x.get('type')=='text')
    assert value.get('success') is not False, 'Runtime failed'
    return value.get('output',value)
init()
inventory=output(call('plugin_tool',{'action':'list','runner':c['runner'],'plugin':'mycodex-tools'}))
names=sorted(x['name'] for x in inventory['tools'])
assert names==sorted(['docx_read','docx_create','docx_update','xlsx_read','xlsx_create','xlsx_update','pptx_read','pptx_create','pdf_read','pdf_search','pdf_create'])
binding=output(call('plugin_tool',{'action':'describe','runner':c['runner'],'plugin':'mycodex-tools','tool':'docx_create'}))['binding']
output(call('plugin_tool',{'action':'call','binding':binding,'arguments':{'output_path':'desktop-bundle-proof.docx','blocks':[{'type':'paragraph','text':'Desktop bundled tools acceptance'}]}}))
assert (Path(c['root'])/'desktop-bundle-proof.docx').is_file(), 'Plugin did not write to selected project'
args={'project':c['project'],'openaiFileIdRefs':[{'file_id':'test','download_url':'https://127.0.0.1/private','file_name':'guard.png','mime_type':'image/png'}],'output_dir':'artifacts','targets':['guard.png']}
trusted=call('import_conversation_files_to_project',args)
assert trusted.get('isError'), 'Unsafe URL unexpectedly accepted'
trusted_text=json.dumps(trusted).lower()
assert 'trusted oauth' not in trusted_text and 'trusted mcp' not in trusted_text, 'Dedicated token was not trusted'
token=env['WEBCODEX_TOKEN'];session=None;init()
untrusted=call('import_conversation_files_to_project',args)
untrusted_text=json.dumps(untrusted).lower()
assert untrusted.get('isError') and ('trusted oauth' in untrusted_text or 'trusted mcp' in untrusted_text), 'Bootstrap gained file-import trust'
print(json.dumps({'tools':names,'dedicated_token_accepted':True,'bootstrap_rejected':True,'unsafe_url_rejected':True}))
"#;
    let options = serde_json::json!({
        "url": identity.server_url, "runner": runner,
        "project": identity.runtime_project_id, "root": identity.project_path, "env": env_file,
    });
    crate::webcodex::run_test_json(
        &tools.python,
        &["-c".into(), script.into(), options.to_string()],
        None,
        true,
        cancellation,
    )
    .await
}

/// Existing operator choices, even an incomplete choice, are never silently replaced.
pub(crate) fn needs_file_bridge(env_file: &Path) -> DesktopResult<bool> {
    let content = read_config(env_file)?;
    if env_value(&content, TOKEN_FILE_KEY).is_none() && env_value(&content, TRUST_KEY).is_some() {
        return Err(config_error(
            "An existing file-import trust policy requires explicit configuration",
        ));
    }
    Ok(env_value(&content, TOKEN_FILE_KEY).is_none())
}

pub(crate) fn install_file_bridge(env_file: &Path, bridge: &BridgeToken) -> DesktopResult<()> {
    if !bridge.token.starts_with("wc_pat_")
        || bridge.token.len() > 8192
        || bridge.token.bytes().any(|b| b <= b' ' || b >= 127)
        || !valid_token_id(&bridge.token_id)
    {
        return Err(config_error(
            "Server returned an invalid local file-bridge credential",
        ));
    }
    let original = read_config(env_file)?;
    if env_value(&original, TOKEN_FILE_KEY).is_some() {
        return Err(config_error(
            "File-bridge configuration changed during setup",
        ));
    }
    let directory = env_file
        .parent()
        .ok_or_else(|| config_error("Invalid Server configuration path"))?
        .join("file-bridge");
    std::fs::create_dir_all(&directory)
        .map_err(|_| config_error("Could not prepare local file-bridge storage"))?;
    protect_secret_directory(&directory)?;
    let token_path = directory.join(format!("{}.token", bridge.token_id));
    if token_path.exists() {
        return Err(config_error(
            "The new file-bridge credential target already exists",
        ));
    }
    crate::state::write_atomic_file(&token_path, bridge.token.as_bytes())
        .map_err(|_| config_error("Could not save the local file-bridge credential"))?;
    let updated = bridge_env_content(&original, &token_path, &bridge.token_id)?;
    if read_config(env_file)? != original {
        return Err(config_error(
            "Server configuration changed during file-bridge setup",
        ));
    }
    // Preserve the original env file's private ACL by writing through its existing handle.
    // A single bounded append leaves all prior settings intact.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(env_file)
        .map_err(|_| config_error("Could not update local file-bridge configuration"))?;
    file.write_all(updated.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| config_error("Could not update local file-bridge configuration"))?;
    Ok(())
}

fn bridge_env_content(original: &str, token_path: &Path, token_id: &str) -> DesktopResult<String> {
    if !valid_token_id(token_id) || !token_path.is_absolute() {
        return Err(config_error("Invalid local file-bridge identity"));
    }
    // Preserve explicit trust choices. Replacing an existing assignment by appending
    // would depend on dotenv duplicate-key rules, so refuse instead of broadening it.
    if env_value(original, TRUST_KEY).is_some() {
        return Err(config_error(
            "An existing file-import trust policy requires explicit configuration",
        ));
    }
    let encoded_path = serde_json::to_string(&token_path.to_string_lossy().replace('\\', "/"))
        .map_err(|_| config_error("Invalid local file-bridge credential path"))?;
    Ok(format!(
        "\n{TRUST_KEY}={token_id}\n{TOKEN_FILE_KEY}={encoded_path}\n"
    ))
}

fn valid_token_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn env_value<'a>(content: &'a str, key: &str) -> Option<&'a str> {
    content.lines().find_map(|line| {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        let (name, value) = line.split_once('=')?;
        (name.trim() == key).then_some(value.trim())
    })
}

fn regular_file(path: &Path, limit: Option<u64>) -> DesktopResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| config_error("An installed bundled-tools file is missing"))?;
    if !metadata.is_file() || limit.is_some_and(|limit| metadata.len() > limit) {
        return Err(config_error(
            "A bundled-tools file is not a bounded regular file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(config_error(
                "Bundled-tools files must not be reparse points",
            ));
        }
    }
    Ok(())
}

fn read_config(path: &Path) -> DesktopResult<String> {
    regular_file(path, Some(MAX_CONFIG_BYTES))?;
    std::fs::read_to_string(path)
        .map_err(|_| config_error("Could not read the local runtime configuration"))
}

#[cfg(windows)]
fn protect_secret_directory(path: &Path) -> DesktopResult<()> {
    use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| config_error("Could not inspect credential storage"))?;
    if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
        return Err(config_error(
            "Credential storage must be a local ordinary directory",
        ));
    }
    // Owner Rights grants access to this per-user directory's owner; SYSTEM is
    // retained for normal OS servicing. Children inherit the same private DACL.
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)\0"
        .encode_utf16()
        .collect();
    let mut descriptor = std::ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(config_error(
            "Could not construct private credential permissions",
        ));
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let ok = unsafe {
        SetFileSecurityW(
            wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    unsafe {
        LocalFree(descriptor);
    }
    if ok == 0 {
        return Err(config_error("Could not protect local credential storage"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn protect_secret_directory(path: &Path) -> DesktopResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| config_error("Could not inspect credential storage"))?;
    if !metadata.is_dir() {
        return Err(config_error(
            "Credential storage must be an ordinary directory",
        ));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| config_error("Could not protect local credential storage"))
}

fn config_error(message: &str) -> DesktopError {
    DesktopError::new(
        "bundled_tools_setup_failed",
        message,
        "Check the Desktop installation and local runtime configuration, then retry.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(root: &Path) -> BundledTools {
        BundledTools {
            python: root.join("python.exe"),
            plugin: root.join("plugin.py"),
            font: root.join("font.ttf"),
        }
    }

    #[test]
    fn adds_provider_without_changing_policy_or_other_plugins() {
        let root = std::env::temp_dir();
        let original = "# keep me\n[policy]\nallow_cwd_anywhere = false\n[plugins]\nrequest_timeout_secs = 45\n[[plugins.providers]]\nid = 'other'\ncommand = 'other.exe'\n";
        let tools = bundle(&root);
        let updated = tools.runner_content(original, &root).unwrap();
        assert!(updated.contains("# keep me"));
        let parsed = updated.parse::<DocumentMut>().unwrap();
        assert_eq!(
            parsed["policy"]["allow_cwd_anywhere"].as_bool(),
            Some(false)
        );
        assert_eq!(
            parsed["plugins"]["request_timeout_secs"].as_integer(),
            Some(45)
        );
        assert_eq!(
            parsed["plugins"]["providers"]
                .as_array_of_tables()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(tools.runner_content(&updated, &root).unwrap(), updated);
    }

    #[test]
    fn explicit_provider_is_not_replaced() {
        let root = std::env::temp_dir();
        let original = "[[plugins.providers]]\nid='mycodex-tools'\ncommand='user-python.exe'\nargs=['custom.py']\ncwd='C:/existing'\n";
        assert_eq!(
            bundle(&root).runner_content(original, &root).unwrap(),
            original
        );
    }

    #[test]
    fn bridge_sets_only_exact_managed_id_and_local_path() {
        let token_id = "12345678-1234-1234-1234-123456789abc";
        let path = std::env::temp_dir()
            .join("path with spaces")
            .join("bridge.token");
        let suffix = bridge_env_content("WEBCODEX_TOKEN=old\n", &path, token_id).unwrap();
        assert!(suffix.contains(&format!("{TRUST_KEY}={token_id}")));
        assert!(!suffix.contains("WEBCODEX_TOKEN="));
        assert!(!suffix.contains("CONTROL_PLANE"));
        assert!(bridge_env_content(
            "WEBCODEX_TRUSTED_MCP_FILE_API_TOKEN_IDS=explicit\n",
            &path,
            token_id
        )
        .is_err());
        assert!(bridge_env_content("", &path, "*\nINJECT=yes").is_err());
    }

    #[test]
    fn credential_directory_is_writable_by_its_owner() {
        let root = std::env::temp_dir().join(format!("webcodex-bridge-acl-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        protect_secret_directory(&root).unwrap();
        let path = root.join("test.token");
        crate::state::write_atomic_file(&path, b"test").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"test");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn explicit_bridge_selection_is_preserved_without_reading_its_credential() {
        let path = std::env::temp_dir().join(format!(
            "webcodex-bridge-explicit-{}.env",
            std::process::id()
        ));
        let bytes = "WEBCODEX_TUNNEL_MCP_TOKEN_FILE='Z:/operator/credential.token'\nWEBCODEX_TRUSTED_MCP_FILE_API_TOKEN_IDS=operator-choice\n";
        std::fs::write(&path, bytes).unwrap();
        assert!(!needs_file_bridge(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn saves_only_the_dedicated_pat_and_preserves_the_bootstrap_setting() {
        let root =
            std::env::temp_dir().join(format!("webcodex-bridge-install-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let env_file = root.join("webcodex.env");
        let original =
            "WEBCODEX_TOKEN=fixture-bootstrap\nWEBCODEX_MCP_MODEL_SURFACE=adaptive-runtime-v1\n";
        std::fs::write(&env_file, original).unwrap();
        let bridge = BridgeToken {
            token: "wc_pat_fixture_only".to_string(),
            token_id: "12345678-1234-1234-1234-123456789abc".to_string(),
        };
        assert!(needs_file_bridge(&env_file).unwrap());
        install_file_bridge(&env_file, &bridge).unwrap();
        let installed = std::fs::read_to_string(&env_file).unwrap();
        assert!(installed.starts_with(original));
        assert_eq!(
            env_value(&installed, TRUST_KEY),
            Some(bridge.token_id.as_str())
        );
        assert!(!installed.contains(&bridge.token));
        assert!(!needs_file_bridge(&env_file).unwrap());
        assert!(install_file_bridge(&env_file, &bridge).is_err());
        assert_eq!(std::fs::read_to_string(&env_file).unwrap(), installed);
        let private = root.join("file-bridge");
        let token_file = private.join(format!("{}.token", bridge.token_id));
        assert_eq!(std::fs::read_to_string(&token_file).unwrap(), bridge.token);
        std::fs::remove_file(token_file).unwrap();
        std::fs::remove_dir(private).unwrap();
        std::fs::remove_file(env_file).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn one_time_management_credential_is_removed_when_request_scope_ends() {
        let root =
            std::env::temp_dir().join(format!("webcodex-bridge-once-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let env_file = root.join("webcodex.env");
        std::fs::write(&env_file, "WEBCODEX_TOKEN=fixture-management-only\n").unwrap();
        let path = {
            let credential = bootstrap_credential_file(&env_file).unwrap();
            let path = credential.path().to_owned();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "fixture-management-only"
            );
            path
        };
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(&env_file).unwrap(),
            "WEBCODEX_TOKEN=fixture-management-only\n"
        );
        std::fs::remove_file(env_file).unwrap();
        std::fs::remove_dir(root.join("file-bridge")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn failed_setup_restores_only_its_provider_and_keeps_other_changes() {
        let original = "[policy]\nallow_cwd_anywhere=false\n[[plugins.providers]]\nid='mycodex-tools'\ncwd='old'\n";
        let installed = original.replace("cwd='old'", "cwd='new'");
        assert_eq!(
            rollback_runner_content(original, &installed, &installed).unwrap(),
            original
        );
        let concurrently_edited = installed.replace(
            "allow_cwd_anywhere=false",
            "allow_cwd_anywhere=false\nmax_output_bytes=1234",
        );
        let restored = rollback_runner_content(original, &installed, &concurrently_edited).unwrap();
        assert!(restored.contains("cwd='old'"));
        assert!(restored.contains("max_output_bytes=1234"));
        let user_provider = installed.replace("cwd='new'", "cwd='user-choice'");
        assert_eq!(
            rollback_runner_content(original, &installed, &user_provider).unwrap(),
            user_provider
        );
    }
}
