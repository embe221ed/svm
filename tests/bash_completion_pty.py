"""Run actual Bash Tab completion and assert the arguments executed by Enter.

Run after cargo build/test: python3 tests/bash_completion_pty.py
Uses the shared stdlib terminal harness; no shell command reaches the network.
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

from terminal_workflows import BINARY, Terminal


class BashCompletion(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bash = shutil.which("bash")
        if not cls.bash:
            if os.environ.get("SVM_REQUIRE_PTY_TESTS"):
                raise RuntimeError("bash is required when SVM_REQUIRE_PTY_TESTS is set")
            raise unittest.SkipTest("bash is unavailable")
        if not BINARY.is_file():
            raise RuntimeError(f"Build svm first: {BINARY}")

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="svm-bash-")
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name) / "home with spaces ' quotes"
        versions = self.home / ".svm/versions"
        versions.mkdir(parents=True)
        for name in ("fork:debug", "fork=debug", "my fork", "double\"quote", "single'quote", "$(touch pwned)"):
            (versions / name).mkdir()
        script = self.home / "completion"
        script.write_bytes(subprocess.check_output(
            [str(BINARY), "completions", "bash"],
            cwd=self.home,
            env={**os.environ, "HOME": str(self.home)},
        ))
        self.process = Terminal(
            self.home, ["--noprofile", "--norc", "-i"], executable=self.bash,
            env={"PS1": "SVM_PROMPT> ", "INPUTRC": os.devnull,
                 "COMPLETION_SCRIPT": str(script), "PROMPT_COMMAND": None},
        )
        self.addCleanup(self.process.close)
        self.process.expect("SVM_PROMPT> ")
        self.process.keys(
            b'source "$COMPLETION_SCRIPT"; svm() { printf "%s\\0" "$@" > "$HOME/selected"; }\r'
        )

    def complete(self, line, expected):
        selected = self.home / "selected"
        selected.unlink(missing_ok=True)
        self.process.keys((line + "\t\r").encode())
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if selected.exists():
                actual = selected.read_bytes().split(b"\0")[:-1]
                self.assertEqual(actual, [arg.encode() for arg in expected], self.process.text[-1000:])
                return
            self.process.pump()
        self.fail(f"Completion did not execute {line!r}: {self.process.text[-1000:]!r}")

    def test_colon_and_equals_word_breaks(self):
        self.complete("svm use fork:d", ["use", "fork:debug"])
        self.complete("svm use fork=", ["use", "fork=debug"])

    def test_quotes_and_escaped_spaces(self):
        for prefix in ('"my f', "'my f", r"my\ f"):
            self.complete("svm use " + prefix, ["use", "my fork"])
        self.complete(r'svm use double\"', ["use", 'double"quote'])
        self.complete(r"svm use single\'", ["use", "single'quote"])

    def test_option_before_version(self):
        self.complete(r"svm use --local my\ f", ["use", "--local", "my fork"])

    def test_completion_never_evaluates_version_text(self):
        self.complete("svm use '$(touch", ["use", "$(touch pwned)"])
        self.assertFalse((self.home / "pwned").exists())

    def test_completion_after_shell_operators(self):
        for command in (r"true && svm use my\ f", r":; svm use fork:d", r"false || svm use my\ f", r"printf data | svm use fork:d"):
            expected = "fork:debug" if "fork:d" in command else "my fork"
            self.complete(command, ["use", expected])


if __name__ == "__main__":
    unittest.main(verbosity=2)
