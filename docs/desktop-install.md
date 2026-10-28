# WebCodex Desktop: quick install and ChatGPT connection

**External tools:** Install model CLIs on each Runner computer, sign in with that user’s account or configure their key, then configure and validate the Runner. WebCodex packages integration code and account-independent runtimes, not preconfigured user tool installations. See [tools, accounts and migration](EXTERNAL_TOOLS.md).

[English](desktop-install.md) | [简体中文](desktop-install.zh-CN.md)

For normal Windows or macOS personal use, **WebCodex Desktop + the official OpenAI Secure Tunnel is the recommended path**. It keeps the Server and Runner local, gives ChatGPT a private Tunnel connection, and avoids making first-time users configure reverse proxies, OAuth, system services, or a public WebCodex endpoint.

The normal path is intentionally product-level; you do not need to understand the Runner registry, `runtime_project_id`, the internal MCP authorization file, or launchd internals:

```text
install Desktop
→ save the Tunnel ID and API key in Desktop
→ start Service / Runner without selecting a project
→ start the official OpenAI Secure Tunnel
→ connect ChatGPT with the Tunnel ID
→ Desktop shows “Tunnel ready; waiting for ChatGPT”
→ create a project and authorize its source folders in Projects
→ bind that project to a chat and verify a real task
```

**Important:** “OpenAI Secure Tunnel ready” proves only that the local Tunnel is ready **for** ChatGPT. It does **not** prove that ChatGPT has connected or that a project tool can already execute. The real project read in step 8 is the final proof. For CLI, an existing remote Server, production hosting, or advanced networking, use the [Full Setup](PERSONAL_SETUP.md) or [Deployment](DEPLOYMENT.md) guides instead.

For everyday use after installation, see [Using Desktop](desktop-guide.md). Home separates service status, connection status, and project availability. OpenAI Platform screenshots below are configuration references; follow the current Desktop control names in the text.

## 1. Install WebCodex Desktop

Download the matching Desktop artifact from the [GitHub Releases](https://github.com/yyjeqhc/webcodex/releases) page:

- **Windows:** use the Windows x64 installer.
- **macOS:** use the DMG matching your Mac architecture, Intel or Apple Silicon.

Current macOS builds are ad-hoc signed and are not notarized. If Gatekeeper blocks the first launch of a newly downloaded build, open **System Settings → Privacy & Security → Open Anyway**, then confirm **Open**. Do not disable Gatekeeper globally.

Launch WebCodex Desktop after installation.

**Success looks like:** the main WebCodex window opens without a missing package/runtime error.

**If it fails:** use **Open Anyway** for the Gatekeeper case above; if the packaged runtime is missing, reinstall the same Desktop version instead of manually assembling internal binaries.

**Next:** confirm the Tunnel configuration before starting a Tunnel.

### Background lifecycle and Launch at Login

WebCodex Desktop is a long-running local runtime controller. Closing the main window does **not** quit the application:

- On **macOS**, use the WebCodex menu-bar item to reopen the window.
- On **Windows**, use the WebCodex system-tray icon to reopen the window.
- The Desktop-owned local Server, Runner, regular OpenAI Secure Tunnel, and an active Quick Share session continue running while the window is hidden. Quick Share remains a temporary session; it is not converted into a permanent service by background residency.
- **Stop local runtime** is a desired-state action: it stops the local runtime and changes the saved runtime preference. It is different from hiding the window.
- **Quit WebCodex** is the application-exit action. Quit stops the Desktop-owned process tree before Desktop exits. It does not broadly terminate unrelated WebCodex processes that Desktop does not own.

In **Settings → Background & startup**, **Launch WebCodex at login** registers Desktop with the operating system and starts it in the background without opening the main window. This setting is separate from the saved runtime preferences: `runtime_autostart` still decides whether the saved local runtime is restored, and the saved connection preference still decides whether the regular ChatGPT Tunnel is restored when appropriate.

### Everyday controls

- Use the sidebar to open Home, Projects, Connection, Activity, and Settings. Use **⌘ + 1–5** on macOS or **Ctrl + 1–5** on Windows. Navigation shortcuts also work inside inputs and language selectors; ordinary typing and text-editing shortcuts remain available.
- Home shows service, connection, and project availability separately. **View runtime diagnostics** contains runtime controls.
- In **Activity**, search messages or sources, or select **Warnings and errors only**. Results are newest first. Filtering changes the view without deleting records.
- Use Tab to focus main buttons and Enter to activate them. Navigation moves focus into page content.

## 2. Prepare an OpenAI Tunnel

Create a Tunnel in the OpenAI Platform and prepare an API key that can use that Tunnel:

- [Tunnels - OpenAI API](https://platform.openai.com/settings/organization/tunnels)
- [API keys - OpenAI API](https://platform.openai.com/settings/organization/api-keys)

The Tunnel name is up to you. Record the Tunnel ID. A Restricted API key with only the Tunnel permissions you need is recommended.

![OpenAI Tunnels page](desktop-install/image-20260906171606559.png)

![OpenAI API Keys page](desktop-install/image-20260906171633208.png)

Do not commit or share real API keys, WebCodex tokens, or authorization values.

## 3. Save Tunnel configuration inside Desktop (recommended)

Open **Settings → OpenAI Tunnel network**, or expand **Optional: check ChatGPT tunnel configuration** in **Connection**:

1. Enter your Tunnel ID in **Tunnel ID**.
2. Enter an API key authorized for that Tunnel in the **Tunnel API key** password field.
3. Click **Save configuration**. Once the source shows the local configuration file, you can start the connection. **No Desktop restart is required.**

The same fields are available in the optional Tunnel section during local setup. When a key is already saved, leaving its field blank keeps that key. Desktop never retrieves the secret into the UI; submission clears the input. A failed save retains the Tunnel ID but requires re-entering an unsaved key.

**Priority: complete saved configuration → inherited Desktop process environment.** Desktop never combines a saved Tunnel ID with an environment API key. Saving does not modify system variables or start/stop a connection. New values apply to the next regular OpenAI Tunnel or OpenAI Quick Share start. Stop and start an active connection to apply them.

The file is `secrets/tunnel-config.json` in Desktop's local application data directory:

- macOS: `~/Library/Application Support/dev.webcodex.desktop/secrets/tunnel-config.json`.
- Windows: `%LOCALAPPDATA%\dev.webcodex.desktop\secrets\tunnel-config.json`.

This file contains an **unencrypted API key**. Keep it out of projects, Git, tickets, and shared backups. macOS/Unix writes are owner-only (`0600`); Windows inherits access permissions from the current user's local application data directory. Saves use atomic replacement without retaining old secret backups. The `secrets` directory is excluded by WebCodex’s existing sensitive-path policy. Ordinary `desktop-state.json` remains non-secret runtime state.

**Clear saved configuration and use environment** clears the saved pair and restores environment fallback; the file records `null`. Invalid or unreadable saved configuration does not fall back automatically. Repair it by saving again in the UI or explicitly clear it. Manual file edits require restarting Desktop; in-app saves do not.

### Optional: continue using environment variables

Without saved configuration, Desktop uses its inherited process environment:

```text
CONTROL_PLANE_TUNNEL_ID
CONTROL_PLANE_API_KEY
```

No additional `OPENAI_ADMIN_KEY` or `OPENAI_API_KEY` is needed. On first OpenAI Secure Tunnel use, WebCodex automatically downloads and verifies a pinned `tunnel-client`; manual installation is normally unnecessary. If download fails, check networking or proxies. Advanced users can set `WEBCODEX_TUNNEL_CLIENT_BIN`.

Windows users can set persistent variables for the current user. On macOS, Finder / Dock launches do not read `~/.zshrc`; launch from a Terminal that has loaded the variables or configure the login session environment. After changing variables through this advanced path, use **Quit WebCodex** in the tray and launch it again. Closing the window only hides it and cannot refresh its process environment. **Recheck configuration** neither executes shell startup scripts nor reloads manually edited configuration files.

### macOS Computer Use permissions

For screenshots, window observation, keyboard or pointer control, grant the relevant permissions under **System Settings → Privacy & Security** to the process actually running WebCodex Runner/Desktop. These include **Screen & System Audio Recording**, and **Accessibility** for UI control. Restart the affected process when macOS requires it.

**Success looks like:** the source is the local file and both presence checks pass. Next, select the actual project ChatGPT should use.

## 4. Start services and create projects when needed

Choose **Use WebCodex on this computer** and apply setup. Desktop starts the local Service and Runner without a project. It creates no default management project and grants no implicit HOME directory access. Once both services are ready, the secure tunnel can start.

Before a concrete task, create a named project in **Projects**, explicitly add source folders, and configure the team. Each chat binds its own project. Tasks validate that project and its directory authority; missing projects or permissions reject the task while services and connections remain available.

Home separates service, connection, and project status. Project creation, edits, and removal do not require stopping the global connection. Existing registrations, folder authority, team policies, and chat bindings are preserved. Service recovery no longer requires the old current-project folder to exist. Quick Share remains a separate explicit single-folder sharing workflow and still requires a folder.

**Service startup succeeds when:** Service and Runner are ready. **A project task can execute when:** its chat binding, available folders, and directory authority also pass validation. Connection readiness does not prove task readiness.

Check Activity and error details on failure; do not broaden directory authority to bypass task validation.

## 5. Configure Tunnel networking if needed

Open **Settings → OpenAI Tunnel network**:

- **Automatic (recommended):** use the proxy inherited by Desktop; Windows can also detect the system proxy.
- **Direct:** do not use a proxy.
- **Custom HTTP proxy:** for example `http://127.0.0.1:7890`.

If the Tunnel is already running, stop it, save the new network setting, and start it again. You do **not** need to restart Desktop; each Tunnel start reads the current setting.

**Success looks like:** the network mode saves successfully. If you do not need special proxy handling, leave **Automatic (recommended)** selected.

**If it fails:** fix Tunnel networking here; do not change project authority or Runner configuration to work around a network error.

**Next:** start the OpenAI Secure Tunnel.

## 6. Start the official OpenAI Secure Tunnel

Open **Connection**, select **OpenAI Secure Tunnel**, then click **Start secure tunnel**. Selection alone does not start or stop processes. If an existing tunnel reports an error, stop it before starting again; failures remain visible with a retry path. When local handoff is ready, Desktop should show wording such as:

> OpenAI Secure Tunnel ready; waiting for ChatGPT

Desktop copies the Tunnel ID to the clipboard when the operating system allows it.

Desktop must **not** promote daemon readiness or a successful clipboard copy to “ChatGPT connected” or “Ready to use”. Those facts prove only that the Tunnel is **ready for ChatGPT**.

**Success looks like:** the Tunnel itself is locally ready while the overall presentation still makes clear that ChatGPT connection/final verification is pending.

**If it fails:** first confirm that both configuration-presence fields from step 3 are Detected, then check the proxy mode in step 5 and use the on-screen Tunnel recovery action.

**Next:** enter the Tunnel ID in ChatGPT.

## 7. Add WebCodex to ChatGPT

When creating the custom connection/app in ChatGPT:

1. Choose **Tunnel** as the connection method.
2. Enter the Tunnel ID from Desktop/OpenAI.
3. Set **Authentication** to **None / No authentication**.

You do not configure OAuth in ChatGPT for this path. WebCodex keeps the MCP authorization credential locally and the Tunnel client injects it; ChatGPT does not need the local credential.

After saving the ChatGPT connection, return to Desktop. Saving a connection in ChatGPT does not magically upgrade local Tunnel evidence into an authoritative “connected” signal. If this Desktop version has no stable external-MCP-client observation signal, it intentionally continues to say that it is waiting for ChatGPT.

**Success looks like:** the ChatGPT connection is saved and the Desktop Tunnel remains running.

**If it fails:** verify that you entered the Tunnel ID, not the API key; Authentication should be None / No authentication. Do not paste the Tunnel API key into ChatGPT.

**Next:** immediately perform one real project read.

![Create ChatGPT connection](desktop-install/image-20260906174157920.png)

![Tunnel configuration](desktop-install/image-20260906174207352.png)

![Connected](desktop-install/image-20260906174215647.png)

## 8. Minimal acceptance check

After connecting, start with **one minimal real project read**, for example: “List the WebCodex projects, then list the top-level files in the project I just selected; report an empty directory as empty.” Only this proves the full path is actually working:

- list the WebCodex projects;
- read a file from the project you explicitly added;
- create, read back, and remove one temporary file inside that project;
- run a read-only command such as `git status`, `uname -a`, or `ver`;
- if you enabled Computer Use, list windows or capture a browser window.

If these work, the Tunnel, Server, Runner, project authority, and ordinary tool path are connected correctly.

If Desktop shows a healthy Service / Runner / Project / Tunnel but ChatGPT still cannot list projects or read a file, do **not** treat local green status as proof of an external connection. Recheck the Tunnel ID and ChatGPT connection configuration, then inspect Desktop Connection / Activity for the latest state.

## Troubleshooting

**OpenAI Secure Tunnel is disabled:** save a complete Tunnel ID and API key in Desktop, then check that the local runtime is ready. If using environment fallback, fully quit and reopen after changing variables.

**macOS Terminal sees the values but Desktop does not:** Finder/Dock apps do not load `~/.zshrc`; use the Terminal-launch or `launchctl setenv` path above. Desktop Recheck does not execute shell startup scripts.

**Tunnel startup or ChatGPT connection times out:** check **Settings → OpenAI Tunnel network** first, then stop and restart the Tunnel after changing the proxy mode.

**A repository is not accessible:** add that directory explicitly on the **Projects** page with its exact directory authority.

**A project is unavailable:** inspect its source folders and authorization in **Projects**. Service startup and connection do not require a project to be ready.

**I closed the window and reopened it, but new environment variables are still missing:** since the background-lifecycle change, closing the window hides Desktop in the tray/menu bar. Use **Quit WebCodex** there, then start a new process.
