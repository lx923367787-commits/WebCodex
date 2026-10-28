# Using WebCodex Desktop

**External tools:** Install model CLIs on each Runner computer, sign in with that user’s account or configure their key, then configure and validate the Runner. WebCodex packages integration code and account-independent runtimes, not preconfigured user tool installations. See [tools, accounts and migration](EXTERNAL_TOOLS.md).

[English](desktop-guide.md) | [简体中文](desktop-guide.zh-CN.md)

Desktop prepares local projects and manages connections. You ask for work in ChatGPT or another AI client. For installation, Tunnel configuration, and system permissions, see the [installation and connection guide](desktop-install.md).

## First use

1. Choose **Use WebCodex on this computer** on the welcome page, the recommended personal setup.
2. Apply setup to start the Service and Runner. No project or folder selection is required.
3. Tunnel presence checks live under **Optional: check ChatGPT tunnel configuration**. Enter the Tunnel ID and API key and click Save configuration to prefer the local file without restarting. You can also start local services before configuring a Tunnel. If configuration is present, you can opt into connecting ChatGPT after setup.
4. Home separates service, connection, and project status. Before a project task, create a named project in **Projects**, add source folders, and configure its team. Each chat binds its own project.
5. Click **Connection** in the sidebar, select **OpenAI Secure Tunnel**, then click **Start secure tunnel**. Selecting a connection method alone does not start a process.
6. When the tunnel is ready, follow the connection page instructions to enter the Tunnel ID in ChatGPT and perform one real project read.

**Local tunnel readiness does not prove ChatGPT is connected or every project task can execute.** Zero projects does not block connection; task execution still validates its project binding and folder permissions.

If you already have a remote Server, choose the existing Server option on the welcome page, enter its address and pairing code; no folder selection is required to connect. Connection shows the remote Server; its operator manages external connectivity.

For temporary use of one project, choose Quick Share and a connection provider. Quick Share has its own temporary lifecycle, with handoff information and a stop control on Home.

## Start each day on Home

Home separates service readiness, connection state, and project availability. Click **Start** when services are stopped. A ready Service and Runner plus Tunnel configuration are sufficient to start the secure tunnel, even with zero projects.

**Projects** owns named projects and source folders. Home provides status and a management link, without a global project switch. Creating or removing named projects does not stop the global connection. Each chat keeps its own binding and tasks independently validate folder authority. Existing registrations, team settings, and chat bindings survive upgrades.

**View runtime diagnostics** contains runtime mode and stop controls. Projects can be created later; do not broaden directory permissions to bypass task authorization.

## Connections and recovery

Connection shows current status and available controls first, followed by ChatGPT handoff instructions. Missing Tunnel configuration automatically expands presence checks. The section also saves configuration and shows its active source. API keys use a password field; saved values are never returned to the UI.

| Situation | Next action |
| --- | --- |
| Local runtime is not ready | Start or resume services on Home |
| Tunnel ID or API key is missing | Enter and save the Tunnel ID and API key in Desktop; quitting and reopening is needed only for environment-variable changes |
| Start failed with no active tunnel | Fix configuration or networking, then click Start again with the same selection |
| An existing tunnel reports an error | Stop it, then start it again; stop failures remain visible and can be retried |
| Tunnel ready but clipboard handoff failed | Use Copy Tunnel ID on Connection, or select the displayed ID and copy manually; no restart needed |
| Tunnel ready, waiting for ChatGPT | Configure the Tunnel in ChatGPT and list the selected project's top-level files (an empty directory is a valid result) |

**Settings → OpenAI Tunnel network** controls automatic, direct, and custom HTTP proxy modes. Stop a running tunnel before changing its proxy, save, then start it again.

## Activity, settings, and background operation

**Activity** shows newest entries first. Search content or sources, or select **Warnings and errors only**. Filtering never deletes records.

**Settings** contains language, launch at login, Tunnel networking, and diagnostics. You can also switch language at the bottom of the sidebar. Supported languages are 简体中文, English, 日本語, 한국어, Deutsch, and Français. The selection is remembered across restarts, and activity times follow the selected locale. System tray menus and raw backend diagnostics remain in English; the operating system controls native file-picker language.

Closing the window hides it in the menu bar or system tray; the runtime continues in the background. **Quit WebCodex** in the tray ends Desktop and its owned processes. Stopping the Desktop-managed runtime on Home also updates the saved runtime startup preference. These controls do not terminate independently started WebCodex processes.

Use **⌘ + 1–5** on macOS or **Ctrl + 1–5** on Windows to switch between the five pages. Navigation shortcuts also work inside inputs and language selectors; ordinary typing and text-editing shortcuts remain available. Use Tab to focus controls and Enter to activate them; diagnostic disclosure controls also support the keyboard.

Runtime controls on Home and technical diagnostics in Settings are collapsed by default. An explicit stop displays Stopped with a Start action. Activity prioritizes results; enable Show process details for routine process events. Configure Tunnel ID and credentials on Connection; API keys are never displayed.
