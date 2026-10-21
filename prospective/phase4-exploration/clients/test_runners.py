"""No services: check workdir ownership, signal cleanup and exit classification."""
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent


class RunnerTests(unittest.TestCase):
    def test_only_owned_child_is_removed(self):
        with tempfile.TemporaryDirectory() as parent:
            sentinel = pathlib.Path(parent) / "keep.txt"
            sentinel.write_text("do not delete")
            script = '''set -eu
source "$1"
new_workdir "$2"
first="$WORK"
new_workdir "$2"
[[ "$first" != "$WORK" ]]
cleanup_workdir
[[ -d "$first" ]]
WORK="$first"
cleanup_workdir
cleanup_workdir
[[ -f "$2/keep.txt" ]]
'''
            subprocess.run(["bash", "-c", script, "test", str(ROOT / "runner-common.sh"), parent], check=True)
            self.assertEqual(sentinel.read_text(), "do not delete")

    def test_signal_cleanup_preserves_parent(self):
        with tempfile.TemporaryDirectory() as parent:
            script = '''set -eu
source "$1"
new_workdir "$2"
trap cleanup_workdir EXIT
trap 'exit 143' TERM
kill -TERM $$
'''
            p = subprocess.run(["bash", "-c", script, "test", str(ROOT / "runner-common.sh"), parent])
            self.assertEqual(p.returncode, 143)
            self.assertEqual(list(pathlib.Path(parent).iterdir()), [])

    def test_each_invocation_gets_fresh_logs(self):
        with tempfile.TemporaryDirectory() as parent:
            script = '''set -eu
source "$1"
LOG_DIR="$2"
new_logdir ignored
first="$LOGS"
echo old > "$first/old.log"
new_logdir ignored
[[ "$first" != "$LOGS" && -f "$first/old.log" ]]
'''
            subprocess.run(["bash", "-c", script, "test", str(ROOT / "runner-common.sh"), parent], check=True)

    def test_smtp_status_distinguishes_findings_from_failures(self):
        for filename in ["run.sh", "run-round2.sh", "run-round2-fixed.sh"]:
            source = (ROOT / "smtp" / filename).read_text()
            function = "run_step() {" + source.split("run_step() {", 1)[1].split("\n}\n", 1)[0] + "\n}\n"
            for code, text, expected in [
                (0, "ok", 0), (1, "failure", 1), (101, "panic", 1), (124, "timeout", 1),
                (1, "LADDER M1 plans=1 violations=1 baseline_errors=0 exhaustive=true\nLADDER-VIOLATION found", 0),
                (1, "LADDER M1 baseline_errors=1 exhaustive=true\nLADDER-VIOLATION found", 1),
            ]:
                with self.subTest(filename=filename, code=code, text=text), tempfile.TemporaryDirectory() as logs:
                    script = 'LOGS="$1"; FAILED=0\n' + function + '\nrun_step m1-check bash -c \'printf "%s\\n" "$1"; exit "$2"\' test "$2" "$3"\nexit "$FAILED"\n'
                    p = subprocess.run(["bash", "-c", script, "test", logs, text, str(code)], capture_output=True)
                    self.assertEqual(p.returncode, expected, p.stderr.decode())


if __name__ == "__main__":
    unittest.main()
