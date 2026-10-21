"""No VM: reject incomplete or failed multicast harness runs."""
import importlib.util
from pathlib import Path
import unittest
spec = importlib.util.spec_from_file_location("mcast_runner", Path(__file__).with_name("run.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

class CompletionTests(unittest.TestCase):
    def good(self):
        return "\n".join(f"SCENARIO_END name={name} status=0" for name in
            ["normal", "n1", "n2", "n2_cross", "ssm_n2", "invalid", "two_sockets"]) + "\nMCAST_DONE\n"
    def test_complete_findings_are_usable(self):
        self.assertTrue(runner.valid_run(self.good() + "DELIVERY_MISS tag=n2 recv=0 expected=4\n"))
    def test_setup_failure_is_not_success(self):
        self.assertFalse(runner.valid_run("NET_FAIL setup_failed=1\nMCAST_DONE\n"))
    def test_child_failure_is_not_success(self):
        self.assertFalse(runner.valid_run(self.good().replace("status=0", "status=256", 1)))
    def test_missing_child_or_control_failure_is_not_success(self):
        self.assertFalse(runner.valid_run(self.good().replace("name=n1", "name=normal")))
        self.assertFalse(runner.valid_run(self.good() + "CONTROL_FAIL tag=n2 what=unicast\n"))
if __name__ == "__main__":
    unittest.main()
