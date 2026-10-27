import io
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

import cli_options as cli


HELP = """Usage: codebuddy [options]
  --model <model> Model for this session. Currently supported: (first-model, second-model)
  --other <value> Unrelated text (not-a-model)
  --effort <level> Reasoning effort level (low, high, future-effort)
  --help Display help
"""


class DiscoveryTests(unittest.TestCase):
    def discover(self, name="codebuddy", help_text=HELP, version="2.162.0\n"):
        with patch.object(cli, "_resolve", return_value=Path("verified.exe")), patch.object(cli, "_capture", side_effect=[version, help_text]) as capture:
            result = cli.discover_cli_options(name)
        self.assertEqual([call.args[1] for call in capture.call_args_list], ["--version", "--help"])
        return result

    def test_parses_only_installed_help_choices_and_exact_arguments(self):
        result = self.discover()
        self.assertEqual(result["models"], ["first-model", "second-model"])
        self.assertEqual(result["reasoning_efforts"], ["low", "high", "future-effort"])
        self.assertEqual((result["model_argument"], result["reasoning_argument"]), ("--model", "--effort"))
        self.assertEqual(result["status"], "complete")
        self.assertEqual(result["source"], "installed_cli_help")
        self.assertTrue(result["selection_required"])
        self.assertFalse(result["account_availability_verified"])
        self.assertNotIn("help", result)

    def test_supports_wrapped_lists_without_borrowing_another_option(self):
        result = self.discover(help_text=HELP.replace("first-model, second-model", "first-model,\n      second-model"))
        self.assertEqual(result["models"], ["first-model", "second-model"])
        result = self.discover(help_text=HELP.replace("Currently supported: (first-model, second-model)", "See configuration").replace("Unrelated text (not-a-model)", "Currently supported: (wrong-model)"))
        self.assertEqual(result["models"], [])
        self.assertEqual(result["reason"], "help_format_unknown")

    def test_changed_or_ambiguous_help_does_not_invent_options(self):
        for changed in (HELP.replace("Currently supported:", "Available:"),
                        HELP.replace("Reasoning effort level", "Thinking choices"),
                        HELP.replace("second-model", "first-model"),
                        HELP + "\n  --model <model> Currently supported: (third-model)\n",
                        HELP.replace("second-model", "https://host/path?secret=value")):
            with self.subTest(help_text=changed):
                result = self.discover(help_text=changed)
                self.assertEqual(result["status"], "unknown")
                self.assertTrue(result["discovery_incomplete"])
                self.assertEqual(result["reason"], "help_format_unknown")

    def test_other_clis_do_not_inherit_codebuddy_effort_contract(self):
        for name in ("codex", "gemini"):
            with self.subTest(name=name):
                result = self.discover(name=name)
                self.assertEqual(result["status"], "incomplete")
                self.assertEqual(result["models"], [])
                self.assertEqual(result["reasoning_efforts"], [])
                self.assertIsNone(result["reasoning_argument"])
        self.assertEqual(self.discover(name="codex")["reason"], "official_model_catalog_required")

    def test_absent_cli_never_runs_a_command(self):
        with patch.object(cli, "_resolve", return_value=None), patch.object(cli, "_capture") as capture:
            result = cli.discover_cli_options("gemini")
        capture.assert_not_called()
        self.assertFalse(result["installed"])
        self.assertEqual(result["models"], [])
        self.assertEqual(result["reason"], "cli_not_installed")

    def test_unsupported_cli_rejects_before_path_resolution(self):
        with patch.object(cli, "_resolve") as resolve:
            for name in (None, [], "wordbuddy", "codebuddy --print task", "C:\\evil.exe"):
                with self.subTest(name=name), self.assertRaisesRegex(ValueError, "unsupported_cli"):
                    cli.discover_cli_options(name)
        resolve.assert_not_called()

    def test_process_failure_returns_only_fixed_reason(self):
        with patch.object(cli, "_resolve", return_value=Path("verified.exe")), patch.object(cli, "_capture", side_effect=cli.DiscoveryError("process_failed")) as capture:
            result = cli.discover_cli_options("codebuddy")
        self.assertEqual(capture.call_count, 1)
        self.assertEqual(result["reason"], "process_failed")
        self.assertEqual(result["models"], [])

    def test_unrecognized_version_text_is_never_returned(self):
        result = self.discover(version="failed with private account data")
        self.assertIsNone(result["version"])
        self.assertEqual(result["status"], "unknown")
        self.assertNotIn("private", str(result))


class FakeProcess:
    def __init__(self, data=b"2.162.0\n", exit_code=0, timeout=False):
        self.stdout = io.BytesIO(data)
        self.exit_code = exit_code
        self.timeout = timeout
        self.pid = 123

    def wait(self, timeout):
        if self.timeout:
            raise subprocess.TimeoutExpired("redacted", timeout)
        return self.exit_code

    def poll(self):
        return self.exit_code


class ProcessBoundaryTests(unittest.TestCase):
    def test_fixed_argv_without_shell_and_credentials_removed(self):
        process = FakeProcess()
        with patch.dict(os.environ, {"OPENAI_API_KEY": "never-forward", "CODEBUDDY_TOKEN": "never-forward", "NODE_OPTIONS": "never-forward", "PATH": "safe-path"}), patch.object(cli.subprocess, "Popen", return_value=process) as popen:
            self.assertEqual(cli._capture(Path("verified.exe"), "--version"), "2.162.0\n")
        args, kwargs = popen.call_args
        self.assertEqual(args[0], ["verified.exe", "--version"])
        self.assertFalse(kwargs["shell"])
        self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
        self.assertEqual(kwargs["env"]["PATH"], "safe-path")
        self.assertFalse({"OPENAI_API_KEY", "CODEBUDDY_TOKEN", "NODE_OPTIONS"} & set(kwargs["env"]))
        if os.name == "nt":
            self.assertEqual(kwargs["creationflags"], subprocess.CREATE_NO_WINDOW)

    def test_output_cap_kills_process_and_never_returns_partial_text(self):
        with patch.object(cli.subprocess, "Popen", return_value=FakeProcess(b"x" * (cli.MAX_OUTPUT_BYTES + 1))), patch.object(cli, "_stop") as stop:
            with self.assertRaisesRegex(cli.DiscoveryError, "output_limit"):
                cli._capture(Path("verified.exe"), "--help")
        stop.assert_called()

    def test_timeout_kills_and_is_reported_without_command_output(self):
        with patch.object(cli.subprocess, "Popen", return_value=FakeProcess(timeout=True)), patch.object(cli, "_stop") as stop:
            with self.assertRaisesRegex(cli.DiscoveryError, "process_timeout"):
                cli._capture(Path("verified.exe"), "--help")
        stop.assert_called()

    def test_nonzero_exit_and_invalid_encoding_are_fixed_failures(self):
        for process, reason in ((FakeProcess(b"private error", exit_code=1), "process_failed"), (FakeProcess(b"\xff"), "output_encoding_unknown")):
            with self.subTest(reason=reason), patch.object(cli.subprocess, "Popen", return_value=process), self.assertRaisesRegex(cli.DiscoveryError, reason):
                cli._capture(Path("verified.exe"), "--help")

    def test_arbitrary_probe_flag_is_rejected(self):
        with patch.object(cli.subprocess, "Popen") as popen, self.assertRaisesRegex(cli.DiscoveryError, "unsupported_probe"):
            cli._capture(Path("verified.exe"), "--print")
        popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
