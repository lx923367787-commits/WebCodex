# Local pre-publication secret scan / 上传前私密信息检查

Scan locally before committing or publishing. Do not send the repository to an
online scanner and do not print a suspected credential to confirm a match.

## Scan current files and available history

Use a trusted [Gitleaks release](https://github.com/gitleaks/gitleaks/releases),
verify its published checksum, and run from the repository root. The commands
below require Gitleaks on PATH. Output is stored in an ignored local directory.

```bash
mkdir security-scan-output
gitleaks dir . --redact=100 --ignore-gitleaks-allow --no-banner --report-format json --report-path security-scan-output/worktree-redacted.json
gitleaks git . --log-opts="--all --full-history --root" --redact=100 --ignore-gitleaks-allow --no-banner --report-format json --report-path security-scan-output/history-redacted.json
```

The directory command scans local files, including ignored local material.
Large build/dependency trees can make it expensive. If using a configuration to
exclude generated dependencies, keep a record of those exclusions and confirm
that no proposed source files are excluded. Store reports outside the directory
being scanned when practical. `--redact=100` avoids recording credential values
in findings; finding metadata and private paths can still be sensitive.

Gitleaks uses exit code 1 for findings. A match requires review: test fixtures,
ClientWindow identifiers and file-integrity hashes may match generic rules.
Verify the actual purpose locally and record the reason without copying values.
Do not suppress an entire test directory or enable a blanket API-key exception.

Also check sensitive filenames, proposed index contents, credential-bearing Git
remote URLs and repository-local configuration. `.gitignore` prevents new
untracked files from being added normally; it does not erase already tracked
files, old commits, release assets or credentials embedded in a binary.

## If a real credential is found

Stop publication of the affected content. Identify its owner and revoke or
rotate it through that provider. Replace publishable configuration with a safe
example or runtime lookup. Do not edit active local configuration or delete
user data merely to make a scan pass.

If committed history contains the credential, removing it from the working
tree alone is insufficient. Plan history cleanup separately, preserve local
work, and obtain explicit authorization before rewriting published history or
force-pushing. Never validate a suspected live key by sending it to its provider.

## Scope of the 2026-10-06 check

The local audit used Gitleaks 8.30.1 with full redaction. It reviewed current
source files, local configuration and the six commits available in this shallow
checkout. Initial source findings consisted of a synthetic authorization-header
test, deterministic ClientWindow test fixtures in history, and ignored
file-integrity reports. They were reviewed as false positives; no live API
credential was confirmed in that scanned scope.

This is a dated result, not a guarantee for future changes or missing history.
Older history beyond the shallow boundary, binary installers, embedded runtime
databases, dependencies and externally stored credentials require their own
checks if they are later published. The complete local report is maintained in
the parent archive's documentation; it is not required to build this repository.

## 中文要点

只在本机扫描，完整脱敏报告也可能含个人路径，不要直接公开。工作区、暂存内容和
已有历史分别核对；浅克隆未包含的历史不能宣称已检查。命中后先判断是否为真实凭据，
不得为得到“零命中”而删除正常测试或批量放行。真实密钥应先撤销或轮换；删除当前文件
不会清理 Git 历史。上传前还须核对新增文件和实际发布介质。
