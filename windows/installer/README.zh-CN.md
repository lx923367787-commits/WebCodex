# Windows 安装器源码

此目录补入原来放在本机 `.local/installer-build/` 的工具增强版安装器源码：

- `generate_payload.py`：遍历已准备的 Windows 便携目录，生成逐文件安装/卸载清单及 SHA-256 清单。
- `installer.nsi`：NSIS Unicode 安装器，包括 WebView2 检测及补装、当前用户注册表登记、快捷方式、安装目录保护、占用检测和保留新增文档的卸载流程。
- `smoke_installer.py`：命令行静默安装/卸载验收，验证注册项、快捷方式、逐文件哈希、拒绝覆盖和保留新文档。

迁移只将原有构建路径改为命令行参数或 NSIS defines。安装器的品牌、注册项、所有权标记以及安装/卸载行为保留原定义。生成器额外拒绝旧卸载器、旧安装标记及目录联接，避免将已有安装登记错误带到新电脑。

## 构建工具增强版安装包

先按 [Windows 构建说明](../README.md) 生成 `WebCodex Desktop` 便携目录。准备 NSIS 3 Unicode 的 `makensis.exe` 和微软官方的 `MicrosoftEdgeWebview2Setup.exe`。NSIS 编译器和微软引导程序属于外部构建依赖；不需要运行安装器即可编译安装包。

在仓库根目录执行；按新机器实际位置修改参数。输出目录必须位于 payload 外：

```powershell
$payload = (Resolve-Path 'ignore/windows-build-example/WebCodex Desktop').Path
$output = [IO.Path]::GetFullPath('ignore/windows-installer-example')
New-Item -ItemType Directory -Path $output -ErrorAction Stop | Out-Null
python windows/installer/generate_payload.py --payload $payload --output-dir $output
if ($LASTEXITCODE -ne 0) { throw 'Payload generation failed' }
& 'C:\BuildDeps\NSIS\makensis.exe' `
  "/DPAYLOAD_DIR=$payload" `
  "/DGENERATED_DIR=$output" `
  "/DOUTPUT_FILE=$output\WebCodex-Setup-0.4.1-MyTools.exe" `
  '/DBOOTSTRAPPER=C:\BuildDeps\MicrosoftEdgeWebview2Setup.exe' `
  'windows/installer/installer.nsi'
if ($LASTEXITCODE -ne 0) { throw 'NSIS compilation failed' }
```

`APP_ICON` 默认使用仓库 `apps/desktop/src-tauri/icons/icon.ico`，也可传入 `/DAPP_ICON=...`。`/DFAST_SMOKE` 关闭压缩并将诊断写入目标机器的 `%TEMP%\WebCodex-installer-debug-errors.txt`，用于本地调试。正式输出应省略此参数。

payload 只应包含要交付的程序、工具、依赖和许可文件。不要从个人安装目录不加筛选地复制 API Key、Tunnel ID、token、登录缓存、会话数据库、生成的 carrier 或旧安装状态；生成器不是秘密扫描器。构建之后如改动 payload，重新生成清单并编译。

## 可选静默安装/卸载验收

此命令会实际安装程序、写入当前用户注册表和快捷方式，随后运行卸载；应在专门的 Windows 测试用户或测试机上运行。若检测到已有同名注册项或快捷方式，脚本会拒绝操作。它不模拟键鼠，不操作浏览器。

```powershell
python windows/installer/smoke_installer.py `
  "$output\WebCodex-Setup-0.4.1-MyTools.exe" `
  --manifest "$output\payload-manifest.json" `
  --work-dir $output `
  --report "$output\installer-smoke.json"
```

本次源码迁移没有运行这项安装/卸载验收，也没有修改注册表或快捷方式。

## 当前已安装的卸载器与便携包

原电脑现有 `uninstall.exe` 的版本资源为 `WebCodex Desktop 0.4.1`，对应当前用户 `WebCodex Desktop` 卸载注册项，来源为 Tauri 的 NSIS 安装流程。其构建配置已经位于 `apps/desktop/src-tauri/tauri.conf.json`（`bundle.targets = ["nsis"]`、`installMode = "currentUser"`），桌面源码和 Tauri 依赖锁也随仓库保存。

此处补入的历史工具增强版脚本使用独立的 `MyCodexDesktopTools` 注册项及 `.mycodex-install-owner` 标记，与上面那个已安装的卸载器不是同一套安装身份。

不要将旧机器生成的 `uninstall.exe` 和安装标记放入便携包。它们是特定安装流程的状态和清理入口，不是客户端运行所需组件。新电脑若使用便携目录，直接运行 `WebCodex.exe`；若需要 Windows“已安装的应用”登记与卸载入口，应通过新生成的安装包安装，由安装器重新生成对应的卸载器。以上两条路径都保留完整客户端和工具源码。
