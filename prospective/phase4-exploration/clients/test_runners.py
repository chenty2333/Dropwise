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

    def test_new_phase_runners_propagate_failures(self):
        for name in ["gel/run-ladder.sh", "modbus/run-ladder.sh", "pop/run-ladder.sh",
                     "skytable/run-ladder.sh", "skytable/run-deep.sh"]:
            source = (ROOT / name).read_text()
            function = "run_phase() {" + source.split("run_phase() {", 1)[1].split("\n}\n", 1)[0] + "\n}\n"
            with self.subTest(name=name), tempfile.TemporaryDirectory() as logs:
                script = 'LOGS="$1"; STATUS=0\n' + function + '\nrun_phase bad bash -c "exit 7"\nrun_phase good true\nexit "$STATUS"\n'
                result = subprocess.run(["bash", "-c", script, "test", logs], capture_output=True)
                self.assertEqual(result.returncode, 1, result.stderr.decode())

    def test_relative_cargo_builds_use_the_client_directory(self):
        for name in ["gel", "modbus", "pop", "skytable"]:
            source = (ROOT / name / "run-ladder.sh").read_text()
            self.assertLess(source.index('cd "$HERE"'), source.index('cargo build'))

    def test_python_pool_runner_keeps_each_stage_status(self):
        source = (ROOT.parent / "python/pg-mysql/run-aiomysql-pool.sh").read_text()
        body = source[source.index('export PYTHONPATH='):]
        for stage, log in [("--diag", "diag"), ("--levels", None), ("--probe", "probe")]:
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as work:
                stub = pathlib.Path(work) / "python"
                stub.write_text('#!/bin/sh\nfor arg do\n  [ "$arg" != "$FAIL_STAGE" ] || exit 7\ndone\nexit 0\n')
                stub.chmod(0o700)
                script = 'STATUS=0; REPO="$1"; RUN="$1"; PY="$1/python"; PORT=1; DB=test\nexport FAIL_STAGE="$2"\n' + body
                result = subprocess.run(["bash", "-c", script, "test", work, stage], capture_output=True)
                self.assertEqual(result.returncode, 1, result.stderr.decode())
                suffix = "" if log is None else "-" + log
                self.assertIn("exit=7", (pathlib.Path(work) / f"aiomysql-pool{suffix}.txt").read_text())

    def test_postgres_readiness_requires_tcp_and_rejects_timeout(self):
        for name in ["run-fixed.sh", "run-contrast.sh", "run-psycopg.sh", "run-asyncpg.sh"]:
            source = (ROOT.parent / "python/pg-mysql" / name).read_text()
            wait = 'READY=0\n' + source.split('READY=0\n', 1)[1].split('\nfi\n', 1)[0] + '\nfi\n'
            for state, expected in [("ready", 0), ("unready", 1)]:
                script = '''NAME=fake; READY_STATE="$1"
docker() { [[ " $* " == *" pg_isready -h 127.0.0.1 "* ]] && [[ "$READY_STATE" == ready ]]; }
sleep() { :; }
''' + wait
                with self.subTest(name=name, state=state):
                    result = subprocess.run(["bash", "-c", script, "test", state], capture_output=True)
                    self.assertEqual(result.returncode, expected, result.stderr.decode())


if __name__ == "__main__":
    unittest.main()
