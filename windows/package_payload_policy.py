"""Reject known user-owned state and model executors from distributable payloads.

This path gate is not a general secret scanner. Never package a live installation.
"""
from pathlib import Path, PurePosixPath


PRIVATE_PARTS = frozenset({
    '.git', '.local', '.venv', '.codex', '.gemini', '.codebuddy', '.ssh', '.aws',
    '.azure', 'provider-config', 'secrets', 'user data', 'browser-profile',
})
PRIVATE_FILES = frozenset({
    'auth.json', 'credentials.json', 'control.json', 'desktop-state.json',
    'runner.toml', 'webcodex.db', 'cookies', 'login data', '.npmrc', '.pypirc',
    'id_rsa', 'id_ed25519',
})


def validate_payload_path(value: str | Path, *, source: Path | None = None) -> None:
    path = PurePosixPath(str(value).replace('\\', '/'))
    parts = tuple(part.casefold() for part in path.parts)
    if path.is_absolute() or '..' in parts or ':' in str(path):
        raise ValueError(f'Unsafe payload path: {path}')
    name = parts[-1] if parts else ''
    # The official Windows Node distribution contains an empty npm/.npmrc.
    # Permit only that empty built-in file, never a configured npm registry/token.
    empty_builtin_npmrc = (name == '.npmrc' and source is not None
                          and source.as_posix().casefold().endswith('/node_modules/npm/.npmrc')
                          and source.is_file() and source.stat().st_size == 0)
    if (PRIVATE_PARTS.intersection(parts) or (name in PRIVATE_FILES and not empty_builtin_npmrc)
            or name.endswith('.local.toml')
            or (name.startswith('.env') and name not in {'.env.example', '.env.sample'})):
        raise ValueError(f'User configuration or account state is not distributable: {path}')
    if (parts[:1] == ('cli',)
            or parts[:2] == ('goal-provider', 'node_modules')
            or any(part in {'codex', 'codebuddy-code', 'codex-acp', 'gemini-cli'} or part.startswith('codex-win32-')
                   for part in parts if 'node_modules' in parts)
            or name in {'codex.exe', 'codebuddy.exe', 'agy.exe', 'gemini.exe'}):
        raise ValueError(f'Install model executors on the destination computer: {path}')


def validate_payload_tree(root: Path) -> None:
    import os
    import stat
    if not root.is_dir():
        raise ValueError(f'Missing payload directory: {root}')
    for ancestor in (root, *root.parents):
        attrs = ancestor.lstat()
        if stat.S_ISLNK(attrs.st_mode) or getattr(attrs, 'st_file_attributes', 0) & 0x400:
            raise ValueError(f'Reparse point is not distributable: {ancestor}')
    for current, directories, files in os.walk(root, followlinks=False):
        for name in directories + files:
            path = Path(current) / name
            attrs = path.lstat()
            if stat.S_ISLNK(attrs.st_mode) or getattr(attrs, 'st_file_attributes', 0) & 0x400:
                raise ValueError(f'Reparse point is not distributable: {path.relative_to(root)}')
            validate_payload_path(path.relative_to(root), source=path)


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('payload', type=Path)
    args = parser.parse_args()
    if not args.payload.is_dir() or args.payload.is_symlink():
        parser.error('Expected a regular payload directory')
    validate_payload_tree(args.payload)
    print('Payload path policy passed; a separate content review is still required.')
