import importlib.util
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from package_payload_policy import validate_payload_path, validate_payload_tree


WINDOWS = Path(__file__).resolve().parent


def load_packager():
    spec = importlib.util.spec_from_file_location('package_home', WINDOWS / 'package-home.py')
    package = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(package)
    return package


class PackagePolicyTests(unittest.TestCase):
    def test_account_state_and_model_executors_are_rejected(self):
        for path in ('cli/codex/bin/codex.js', 'CLI/agy/agy.exe',
                     'goal-provider/node_modules/@openai/codex/package.json',
                     'node/node_modules/@tencent-ai/codebuddy-code/bin/codebuddy',
                     'node/node_modules/@google/gemini-cli/package.json',
                     '.codex/auth.json', 'provider-config/runner.local.toml',
                     'secrets/tunnel.json', 'node/.npmrc', '.env.production',
                     'browser/User Data/Default/Cookies', 'runtime/webcodex.db'):
            with self.subTest(path=path), self.assertRaises(ValueError):
                validate_payload_path(path)

    def test_source_examples_and_shared_runtimes_are_allowed(self):
        for path in ('goal-provider/start.mjs', 'goal-provider/package-lock.json',
                     'goal-provider/codex.acp.example.toml', 'node/node.exe',
                     'mycodex-tools/python/python.exe', 'tunnel/tunnel-client.exe',
                     '.env.example', 'runner.example.toml'):
            validate_payload_path(path)

    def test_copy_tree_rejects_private_state_before_copy(self):
        package = load_packager()
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'source'
            target = Path(temp) / 'target'
            source.mkdir()
            (source / 'normal.txt').write_text('public')
            (source / 'auth.json').write_text('private fixture')
            with self.assertRaises(ValueError):
                package.copy_tree(source, target)
            self.assertFalse(target.exists())

    def test_installer_style_tree_rejects_nested_executor(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'goal-provider/node_modules').mkdir(parents=True)
            with self.assertRaises(ValueError):
                validate_payload_tree(root)

    def test_manifest_cannot_authorize_private_state(self):
        package = load_packager()
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'source'
            source.mkdir()
            private = source / 'auth.json'
            private.write_bytes(b'private fixture')
            (source / 'manifest.json').write_text(json.dumps({'files': {
                'auth.json': hashlib.sha256(private.read_bytes()).hexdigest()}}))
            target = Path(temp) / 'target'
            with self.assertRaisesRegex(ValueError, 'not distributable'):
                package.copy_manifest_payload(source, 'manifest.json', target)
            self.assertFalse(target.exists())

    def test_missing_tree_is_not_accepted(self):
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaises(ValueError):
                validate_payload_tree(Path(temp) / 'missing')

    def test_only_empty_builtin_npm_config_can_be_copied(self):
        package = load_packager()
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'node'
            config = source / 'node_modules/npm/.npmrc'
            config.parent.mkdir(parents=True)
            config.touch()
            package.copy_tree(source, Path(temp) / 'target')
            self.assertEqual((Path(temp) / 'target/node_modules/npm/.npmrc').read_bytes(), b'')
            config.write_text('registry=https://example.invalid\n')
            with self.assertRaises(ValueError):
                package.copy_tree(source, Path(temp) / 'rejected')
            self.assertFalse((Path(temp) / 'rejected').exists())

    @unittest.skipUnless(os.name == 'nt', 'Windows installer')
    def test_installer_rejects_private_state_before_generating(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'source'
            source.mkdir()
            (source / 'auth.json').write_text('private fixture')
            target = Path(temp) / 'generated'
            result = subprocess.run([sys.executable, '-B', str(WINDOWS / 'installer/generate_payload.py'),
                                     '--payload', str(source), '--output-dir', str(target)],
                                    capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=30)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('not distributable', result.stderr)
            self.assertFalse(target.exists())

    @unittest.skipUnless(os.name == 'nt' and shutil.which('pwsh'), 'Windows PowerShell')
    def test_goal_template_is_opt_in_and_requires_local_install(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for relative in ('chat-handoff/edge-handoff.provider.example.toml',
                             'chat-agents/chat-agents.provider.example.toml',
                             'goal-provider/codex.acp.example.toml'):
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(WINDOWS / relative, target)
            runtime = root / 'runtime.exe'
            runtime.touch()  # The generator resolves paths; it must never execute these files.
            output = root / 'generated'
            command = [shutil.which('pwsh'), '-NoProfile', '-File', str(WINDOWS / 'configure-providers.ps1'),
                       '-ToolRoot', str(root), '-Python', str(runtime), '-Node', str(runtime),
                       '-OutputDirectory', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual({path.name for path in output.iterdir()},
                             {'edge-handoff.provider.local.toml', 'chat-agents.provider.local.toml'})
            result = subprocess.run(command + ['-IncludeGoalProvider'], capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=30)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Install the optional Goal ACP dependencies', result.stderr)
            self.assertFalse((output / 'codex.acp.local.toml').exists())
            entry = root / 'goal-provider/node_modules/@agentclientprotocol/codex-acp/dist/index.js'
            entry.parent.mkdir(parents=True)
            entry.touch()
            result = subprocess.run(command + ['-IncludeGoalProvider'], capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((output / 'codex.acp.local.toml').is_file())


if __name__ == '__main__':
    unittest.main()
