"""compat_paired_host_report.py over a real on-disk layout of both lanes (#251)."""

import json
import subprocess
import sys
import tempfile
import unittest

import yaml
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import compat_paired_host_report as host  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
RUN, PLATFORM = "4242", "Linux-X64"


class HostReportTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        root = Path(self.dir.name)
        self.temp, self.official, self.home = root / "temp", root / "official", root / "home"
        for path in (self.temp / "toolu-home/_diag/jobs", self.official / "_diag",
                     self.home / ".toolu-runner/_diag", self.temp / "toolu-ephemeral"):
            path.mkdir(parents=True)
        self.toolu_value = host.disposable(RUN, PLATFORM, "toolu")
        self.official_value = host.disposable(RUN, PLATFORM, "official")

    def tearDown(self):
        self.dir.cleanup()

    def run_report(self, *extra):
        out = Path(self.dir.name) / "host.json"
        host.main(["--out", str(out), "--platform", PLATFORM, "--run-id", RUN, "--temp", str(self.temp),
                   "--official-dir", str(self.official), "--home", str(self.home), *extra])
        return json.loads(out.read_text())

    def test_disposable_matches_the_probe_derivation(self):
        # The `leaks` job's own command line, run with this test's values, so
        # a change in the probe's derivation cannot leave the scan blind.
        probe = yaml.safe_load((ROOT / ".github/workflows/compat-reference-103-probe.yml").read_text())
        script = probe["jobs"]["leaks"]["steps"][0]["run"]
        line = next(l.strip() for l in script.splitlines() if l.strip().startswith("value=$("))
        for expr, value in (("${{ github.run_id }}", RUN), ("${{ inputs.lane }}", f"{PLATFORM}-toolu"),
                            ("${{ inputs.variant }}", "probe")):
            self.assertIn(expr, line)
            line = line.replace(expr, value)
        shell = subprocess.run(["sh", "-c", f'{line}; printf %s "$value"'],
                               check=True, capture_output=True, text=True).stdout
        self.assertEqual(self.toolu_value, shell)
        self.assertNotEqual(self.toolu_value, self.official_value)

    def test_clean_lanes_count_nothing(self):
        (self.temp / "toolu.log").write_text("INFO acquired job\n")
        (self.official / "_diag/Runner_1.log").write_text("Listening for Jobs\n")
        lanes = self.run_report()["lanes"]
        self.assertEqual(lanes["toolu"], {"disposable_hits": 0, "renewals_max": 0, "poll_failures": 0})
        self.assertEqual(lanes["official"], {"disposable_hits": 0, "renewals_max": 0, "poll_failures": 0})

    def test_a_leak_is_counted_only_for_its_own_lane(self):
        (self.home / ".toolu-runner/_diag/runner.log").write_text(f"x {self.toolu_value} y\n")
        (self.temp / "toolu-home/_diag/jobs/1-job.jsonl").write_text(self.toolu_value)
        (self.official / "_diag/Worker_1.log").write_text(f"{self.toolu_value}\n")
        lanes = self.run_report()["lanes"]
        self.assertEqual(lanes["toolu"]["disposable_hits"], 2)
        self.assertEqual(lanes["official"]["disposable_hits"], 0)

    def test_renewals_are_the_most_any_one_job_recorded(self):
        jobs = self.temp / "toolu-home/_diag/jobs"
        (jobs / "1-a.jsonl").write_text('{"type":"lock_renewed"}\n' * 3)
        (jobs / "2-b.jsonl").write_text('{"type":"lock_renewed"}\n')
        (self.official / "_diag/Runner_1.log").write_text("Successfully renew job a\n" * 2)
        (self.official / "_diag/Runner_2.log").write_text("Successfully renew job b\n" * 4)
        (self.official / "_diag/Worker_2.log").write_text("Successfully renew job b\n" * 9)
        lanes = self.run_report()["lanes"]
        self.assertEqual(lanes["toolu"]["renewals_max"], 3)
        self.assertEqual(lanes["official"]["renewals_max"], 4)

    def test_poll_failures_are_counted_per_lane(self):
        (self.temp / "toolu.log").write_text("WARN poll failed — retrying after backoff error=message poll returned status 404\n" * 5)
        (self.official / "_diag/Runner_1.log").write_text("Catch exception during get next message\n")
        lanes = self.run_report()["lanes"]
        self.assertEqual(lanes["toolu"]["poll_failures"], 5)
        self.assertEqual(lanes["official"]["poll_failures"], 1)

    def test_ephemeral_state(self):
        exit_file = self.temp / "ephemeral.exit"
        exit_file.write_text("0\n")
        report = self.run_report("--ephemeral-exit", str(exit_file), "--ephemeral-runner-gone", "true")
        self.assertEqual(report["toolu_only"]["ephemeral"],
                         {"exit_code": 0, "state_removed": True, "runner_removed": True})
        (self.temp / "toolu-ephemeral/config.toml").write_text("kept")
        report = self.run_report("--ephemeral-runner-gone", "false")
        self.assertEqual(report["toolu_only"]["ephemeral"],
                         {"exit_code": None, "state_removed": False, "runner_removed": False})


if __name__ == "__main__":
    unittest.main()
