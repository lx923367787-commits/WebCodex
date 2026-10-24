"""Save the explicitly user-copied extension token locally, without echoing it."""
import base64
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
TARGET = ROOT / 'runtime' / 'playwright-extension-token.txt'


def ps(script):
    result = subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
        '$ErrorActionPreference="Stop";[Console]::OutputEncoding=[Text.UTF8Encoding]::new();'+script],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=8,
        creationflags=subprocess.CREATE_NO_WINDOW, check=True)
    if len(result.stdout) > 4096:
        raise ValueError('unexpected_clipboard_format')
    return result.stdout.decode('utf-8-sig').strip()


def main():
    if sys.argv[1:] != ['--user-copied-token']:
        raise ValueError('explicit_copy_confirmation_required')
    copied = ps('Get-Clipboard -Raw')
    prefix = 'PLAYWRIGHT_MCP_EXTENSION_TOKEN='
    if not copied.startswith(prefix):
        raise ValueError('copy_the_named_extension_token')
    token = copied[len(prefix):]
    if not re.fullmatch(r'[A-Za-z0-9_-]{43}', token):
        raise ValueError('unexpected_clipboard_format')
    if len(base64.urlsafe_b64decode(token+'=')) != 32:
        raise ValueError('unexpected_clipboard_format')
    TARGET.parent.mkdir(parents=True, exist_ok=True)
    if TARGET.resolve() != TARGET.absolute() or TARGET.exists():
        raise ValueError('token_file_already_exists_or_redirected')
    sid = ps('[Security.Principal.WindowsIdentity]::GetCurrent().User.Value')
    if not re.fullmatch(r'S-1-\d+(?:-\d+)+',sid):
        raise ValueError('identity_unavailable')
    with TARGET.open('x', encoding='utf-8') as output:
        subprocess.run(['icacls.exe',str(TARGET),'/inheritance:r','/grant:r',f'*{sid}:(F)',
            '/grant:r','*S-1-5-18:(F)'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,
            timeout=8,check=True,creationflags=subprocess.CREATE_NO_WINDOW)
        output.write(token+'\n')
        output.flush()
        os.fsync(output.fileno())
    print(json.dumps({'saved':True,'restricted_to_current_user_and_system':True}))


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print(json.dumps({'saved':False,'reason':'token_capture_failed'}))
        raise SystemExit(2)
