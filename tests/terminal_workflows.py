"""Exercise real fzf keyboard workflows in a terminal using only the stdlib.

Run after cargo build/test: python3 tests/terminal_workflows.py
Set SVM_REQUIRE_PTY_TESTS=1 to fail instead of skip if fzf is unavailable.
"""

import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import tempfile
import termios
import time
import unittest


ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("SVM_TEST_BINARY", ROOT / "target/debug/svm"))
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
API = "https://api.github.com/repos/MystenLabs/sui/releases"


class Terminal:
    def __init__(self, home, args, *, env=None, stdin_tty=True, columns=120, executable=None):
        executable = Path(executable) if executable is not None else BINARY
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                os.chdir(home)
                fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, columns, 0, 0))
                if not stdin_tty:
                    with open(os.devnull, "rb") as source:
                        os.dup2(source.fileno(), 0)
                child_env = os.environ.copy()
                for name in (
                    "SVM_API_BASE", "SVM_DOWNLOAD_BASE", "GITHUB_TOKEN", "SVM_AUTO_INSTALL",
                    "FZF_DEFAULT_OPTS", "FZF_DEFAULT_OPTS_FILE", "FZF_DEFAULT_COMMAND",
                ):
                    child_env.pop(name, None)
                child_env.update(HOME=str(home), NO_COLOR="1", TERM="xterm-256color")
                for name, value in (env or {}).items():
                    if value is None:
                        child_env.pop(name, None)
                    else:
                        child_env[name] = value
                os.execve(executable, [str(executable), *args], child_env)
            except BaseException:
                os._exit(127)
        self.output = bytearray()
        self.status = None

    @property
    def text(self):
        return ANSI.sub("", self.output.decode(errors="replace"))

    def pump(self, timeout=0.05):
        if select.select([self.fd], [], [], timeout)[0]:
            try:
                chunk = os.read(self.fd, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                return False
            if not chunk:
                return False
            self.output.extend(chunk)
        return True

    def expect(self, text):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if text in self.text:
                return
            if not self.pump():
                break
        raise AssertionError(f"Missing terminal text {text!r}: {self.text[-1500:]!r}")

    def keys(self, keys):
        os.write(self.fd, keys)
        # Give fzf time to render between distinct user actions. No network
        # latency is involved: test candidates are already installed/cached.
        deadline = time.monotonic() + 0.1
        while time.monotonic() < deadline:
            self.pump(0.01)

    def finish(self):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            alive = self.pump()
            done, status = os.waitpid(self.pid, os.WNOHANG)
            if done:
                self.status = os.waitstatus_to_exitcode(status)
                drain_deadline = time.monotonic() + 0.25
                while alive and time.monotonic() < drain_deadline:
                    alive = self.pump(0.01)
                if self.status != 0:
                    raise AssertionError(f"svm exited {self.status}: {self.text[-1500:]!r}")
                return self.text
        raise AssertionError(f"Terminal command timed out: {self.text[-1500:]!r}")

    def close(self):
        # A failed test can leave fzf holding the PTY open after svm exits.
        # pty.fork gives the child its own process group; clean up that group.
        try:
            os.killpg(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        if self.status is None:
            try:
                os.waitpid(self.pid, 0)
            except ChildProcessError:
                pass
        os.close(self.fd)


class TerminalWorkflows(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not BINARY.is_file():
            raise RuntimeError(f"Build svm first: {BINARY}")
        if not shutil.which("fzf"):
            if os.environ.get("SVM_REQUIRE_PTY_TESTS"):
                raise RuntimeError("fzf is required when SVM_REQUIRE_PTY_TESTS is set")
            raise unittest.SkipTest("fzf is unavailable")

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="svm-terminal-")
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.svm = self.home / ".svm"
        for tag in ("mainnet-v1.2.3", "mainnet-v1.2.30"):
            version = self.svm / "versions" / tag
            version.mkdir(parents=True)
            binary = version / "sui"
            binary.write_text("#!/bin/sh\necho terminal-test\n")
            binary.chmod(0o755)
        cache = self.svm / "cache"
        cache.mkdir()
        (cache / "releases.json").write_text(json.dumps({
            "source": API, "etag": None, "pages": 1,
            "releases": [{"tag_name": tag} for tag in ("mainnet-v1.2.3", "mainnet-v1.2.30")],
        }))

    def terminal(self, args, **kwargs):
        process = Terminal(self.home, args, **kwargs)
        self.addCleanup(process.close)
        return process

    def test_menu_cancellation_does_not_activate(self):
        process = self.terminal(["interactive"])
        process.expect("SVM |")
        process.keys(b"\x03")
        process.finish()
        self.assertFalse((self.svm / "version").exists())

    def test_switch_global_uses_selected_installed_version(self):
        process = self.terminal(["interactive"])
        process.expect("SVM |")
        process.keys(b"\r")
        process.expect("Set global default")
        process.keys(b"\x1b[B")
        process.keys(b"\r")
        process.finish()
        self.assertEqual((self.svm / "version").read_text(), "mainnet-v1.2.3\n")

    def test_installed_picker_escape_leaves_selection_unchanged(self):
        (self.svm / "version").write_text("mainnet-v1.2.30\n")
        process = self.terminal(["interactive"])
        process.expect("SVM |")
        process.keys(b"\r")
        process.expect("Set global default")
        process.keys(b"\x1b")
        process.finish()
        self.assertEqual((self.svm / "version").read_text(), "mainnet-v1.2.30\n")

    def test_pin_local_leaves_global_setting(self):
        (self.svm / "version").write_text("mainnet-v1.2.3\n")
        process = self.terminal(["interactive"])
        process.expect("SVM |")
        process.keys(b"\x1b[B")
        process.keys(b"\r")
        process.expect("Pin in this directory")
        process.keys(b"\r")
        process.finish()
        self.assertEqual((self.home / ".svm-version").read_text(), "mainnet-v1.2.30\n")
        self.assertEqual((self.svm / "version").read_text(), "mainnet-v1.2.3\n")

    def test_remote_picker_preserves_prefix_collision(self):
        process = self.terminal(["remote-list", "--cached"])
        process.expect("Releases |")
        process.keys(b"\x1b[B")
        process.keys(b"\r")
        output = process.finish()
        self.assertTrue(output.rstrip().endswith("mainnet-v1.2.30"), output[-500:])

    def test_missing_fzf_falls_back_to_plain_table(self):
        process = self.terminal(["remote-list", "--cached"], env={"PATH": str(self.home)})
        output = process.finish()
        self.assertIn("fzf is not installed", output)
        self.assertIn("mainnet-v1.2.30", output)

    def test_redirected_stdin_uses_table_even_with_tty_stdout(self):
        process = self.terminal(["remote-list", "--cached"], stdin_tty=False)
        output = process.finish()
        self.assertIn("Version", output)
        self.assertIn("mainnet-v1.2.30", output)
        self.assertNotIn("Releases |", output)

    def test_narrow_terminal_cancellation(self):
        process = self.terminal(["interactive"], columns=40)
        process.expect("Switch global")
        process.keys(b"\x03")
        process.finish()
        self.assertFalse((self.svm / "version").exists())

    def test_clicolor_zero_disables_plain_table_colors(self):
        process = self.terminal(
            ["remote-list", "--cached", "--plain"],
            env={"NO_COLOR": None, "CLICOLOR": "0", "CLICOLOR_FORCE": "0"},
        )
        process.finish()
        self.assertNotIn(b"\x1b", process.output)


if __name__ == "__main__":
    unittest.main(verbosity=2)
