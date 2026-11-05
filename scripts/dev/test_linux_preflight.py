"""Exercise preflight parsing and platform admission without Linux or credentials."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("linux-preflight.sh").resolve()


class PreflightTests(unittest.TestCase):
    def linux_fixture(self, directory, rust="1.89.0", missing=None, pwa=False):
        root = Path(directory)
        log = root / "commands.log"
        fake_bin = root / "bin"
        fake_bin.mkdir()
        for name, body in {
            "uname": "printf 'Linux\\n'",
            "git": "printf 'git version 2.50.0\\n'",
            "rustc": f"printf 'rustc {rust} (fixture)\\n'",
            "cargo": "case \"$*\" in '--version') printf 'cargo fixture\\n';; 'fmt --version') printf 'rustfmt fixture\\n';; 'clippy --version') printf 'clippy fixture\\n';; *) exit 99;; esac",
            "bash": "exit 99",
            "make": "exit 99",
            "cc": "exit 99",
            **({"node": "printf 'v22.0.0\\n'", "npm": "printf '10.0.0\\n'"} if pwa else {}),
        }.items():
            if name == missing:
                continue
            program = fake_bin / name
            program.write_text(f"#!/bin/sh\nprintf '%s\\n' \"{name} $*\" >> \"$PREFLIGHT_TEST_CALLS\"\n{body}\n")
            program.chmod(0o700)
        # Substitute the fixed Git path only in this isolated test copy. The real
        # script has no runtime override for its /usr/bin/git requirement.
        script = root / "preflight.sh"
        script.write_text(SCRIPT.read_text().replace("/usr/bin/git", str(fake_bin / "git")))
        return script, log, {
            "PATH": str(fake_bin),
            "PREFLIGHT_TEST_CALLS": str(log),
            "HOME": str(root),
        }

    def version(self, function, value):
        result = subprocess.run(
            ["bash", "-c", 'source "$1"; "$2" "$3"', "test", str(SCRIPT), function, value],
            capture_output=True, text=True,
        )
        return result.returncode

    def test_git_uses_the_required_version_boundary(self):
        for value in ["git version 2.50.0", "git version 2.50.1 (Apple Git-155)", "git version 2.51.0", "git version 3.0.0"]:
            self.assertEqual(self.version("git_version_supported", value), 0, value)
        for value in ["git version 2.49.9", "git version 1.99.0", "not a version"]:
            self.assertNotEqual(self.version("git_version_supported", value), 0, value)

    def test_rust_uses_the_validated_baseline(self):
        for value in ["rustc 1.89.0 (fixture)", "rustc 1.90.0 (fixture)", "rustc 2.0.0"]:
            self.assertEqual(self.version("rust_version_supported", value), 0, value)
        for value in ["rustc 1.88.0", "rustc 0.99.0", "invalid"]:
            self.assertNotEqual(self.version("rust_version_supported", value), 0, value)

    def test_help_and_unknown_options_need_no_toolchain(self):
        help_result = subprocess.run(["bash", str(SCRIPT), "--help"], capture_output=True, text=True)
        self.assertEqual(help_result.returncode, 0)
        self.assertIn("No builds", help_result.stdout)
        invalid = subprocess.run(["bash", str(SCRIPT), "--install"], capture_output=True, text=True)
        self.assertEqual(invalid.returncode, 2)

    def test_non_linux_stops_before_toolchain_or_application_commands(self):
        with tempfile.TemporaryDirectory() as directory:
            fake = Path(directory) / "uname"
            fake.write_text("#!/bin/sh\nprintf 'Darwin\\n'\n")
            fake.chmod(0o700)
            env = dict(os.environ, PATH=directory + os.pathsep + os.environ["PATH"])
            result = subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn("current OS: Darwin", result.stderr)
            self.assertNotIn("OK:", result.stdout)

    def test_mock_linux_success_only_invokes_version_commands(self):
        bash = shutil.which("bash")
        with tempfile.TemporaryDirectory() as directory:
            script, log, env = self.linux_fixture(directory)
            result = subprocess.run([bash, str(script)], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("not a Linux build/runtime validation", result.stdout)
            self.assertEqual(log.read_text().splitlines(), [
                "uname -s", "git --version", "rustc --version", "cargo --version",
                "cargo fmt --version", "cargo clippy --version",
            ])

    def test_mock_linux_missing_tool_and_old_rust_fail(self):
        bash = shutil.which("bash")
        for kwargs, expected in [({"missing": "cc"}, "MISSING: cc"), ({"rust": "1.88.0"}, "Rust >= 1.89")]:
            with self.subTest(kwargs=kwargs), tempfile.TemporaryDirectory() as directory:
                script, _, env = self.linux_fixture(directory, **kwargs)
                result = subprocess.run([bash, str(script)], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 1)
                self.assertIn(expected, result.stderr)

    def test_pwa_is_optional_and_missing_pwa_tools_fail_only_when_requested(self):
        bash = shutil.which("bash")
        for available in [False, True]:
            with self.subTest(available=available), tempfile.TemporaryDirectory() as directory:
                script, _, env = self.linux_fixture(directory, pwa=available)
                result = subprocess.run([bash, str(script), "--pwa"], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0 if available else 1, result.stderr)


if __name__ == "__main__":
    unittest.main()
