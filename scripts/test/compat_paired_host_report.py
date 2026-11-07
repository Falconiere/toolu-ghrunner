#!/usr/bin/env python3
"""One VM's #251 observations for compat-paired.yml, written as host-251.json.

Usage:
  compat_paired_host_report.py --out FILE --platform P --run-id N --temp DIR
                               --official-dir DIR [--home DIR] [--ephemeral-exit FILE]
                               [--ephemeral-runner-gone true|false]

Counts and booleans only; no log line, token or path content leaves the VM.

- `disposable_hits`: files under each lane's own logs and diagnostics that
  contain the lane's disposable value (job `leaks`, probe variant). The value is
  sha256("compat-251-<run>-<platform>-<lane>-probe")[:32], as the probe makes it.
- `renewals_max`: the most lock renewals one job recorded: toolu journal
  `lock_renewed` events per job file; official `Successfully renew job` lines
  per `Runner_*.log` (one listener process per JIT job).
- `poll_failures`: toolu `message poll returned status` WARNs; official
  `Catch exception during get next message` traces.
- `toolu_only.ephemeral`: the ephemeral registration's `run` exit code, whether
  its local state is gone, and whether GitHub no longer lists its runner.
"""

import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

TOOLU_HOMES = ("toolu-home", "toolu-pool-1", "toolu-pool-2", "toolu-fresh", "toolu-ephemeral")
OFFICIAL_POOLS = ("official-pool-1", "official-pool-2")


def disposable(run_id, platform_name, lane):
    return hashlib.sha256(f"compat-251-{run_id}-{platform_name}-{lane}-probe".encode()).hexdigest()[:32]


def files_under(paths):
    for path in paths:
        if path.is_file():
            yield path
        elif path.is_dir():
            yield from (p for p in path.rglob("*") if p.is_file())


def hits(paths, value):
    needle = value.encode()
    return sum(1 for path in files_under(paths) if needle in path.read_bytes())


def count_lines(paths, needle, per_file=False):
    counts = [path.read_bytes().count(needle.encode()) for path in files_under(paths)]
    return max(counts, default=0) if per_file else sum(counts)


def lane_paths(temp, official_dir, home):
    toolu = [temp / f"{name}.log" for name in ("toolu", "toolu-pool-1", "toolu-pool-2", "toolu-fresh",
                                                "toolu-ephemeral", "toolu-accelerated")]
    toolu += [temp / name / "_diag" for name in TOOLU_HOMES]
    toolu += [temp / name / "runners" for name in TOOLU_HOMES]
    toolu.append(home / ".toolu-runner" / "_diag")
    official = [temp / f"{name}.log" for name in ("official",) + OFFICIAL_POOLS]
    official += [official_dir / "_diag"] + [temp / name / "_diag" for name in OFFICIAL_POOLS]
    return {"toolu": toolu, "official": official}


def diag_files(paths, suffix):
    """Only the per-job journal / listener files under `paths`."""
    return [p for p in files_under(paths) if p.name.endswith(suffix)]


def observations(args):
    temp, official_dir = Path(args.temp), Path(args.official_dir)
    paths = lane_paths(temp, official_dir, Path(args.home))
    lanes = {}
    for lane, where in paths.items():
        value = disposable(args.run_id, args.platform, lane)
        lanes[lane] = {"disposable_hits": hits(where, value)}
    lanes["toolu"]["renewals_max"] = count_lines(diag_files(paths["toolu"], ".jsonl"), '"lock_renewed"',
                                                 per_file=True)
    lanes["official"]["renewals_max"] = count_lines(
        [p for p in diag_files(paths["official"], ".log") if p.name.startswith("Runner_")],
        "Successfully renew job", per_file=True)
    lanes["toolu"]["poll_failures"] = count_lines([temp / "toolu.log"], "message poll returned status")
    lanes["official"]["poll_failures"] = count_lines(
        [p for p in diag_files(paths["official"], ".log") if p.name.startswith("Runner_")],
        "Catch exception during get next message")
    return lanes


def ephemeral(args):
    exit_file = Path(args.ephemeral_exit) if args.ephemeral_exit else None
    code = exit_file.read_text().strip() if exit_file and exit_file.is_file() else ""
    home = Path(args.temp) / "toolu-ephemeral"
    return {
        "exit_code": int(code) if code.lstrip("-").isdigit() else None,
        "state_removed": not (home / "config.toml").exists(),
        "runner_removed": args.ephemeral_runner_gone == "true",
    }


def provenance(args):
    def out(*cmd):
        try:
            return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout.strip()
        except (OSError, subprocess.CalledProcessError):
            return None
    toolu_bin = os.environ.get("TOOLU_BIN", "")
    return {
        "image_os": os.environ.get("ImageOS"),
        "image_version": os.environ.get("ImageVersion"),
        "machine": platform.machine(),
        "docker_server": out("docker", "version", "--format", "{{.Server.Version}}"),
        "official_archive": os.environ.get("ARCHIVE"),
        "official_archive_sha256": os.environ.get("SHA256"),
        "source_commit": os.environ.get("GITHUB_SHA"),
        "toolu_build": "cargo build -p toolu-runner --bin toolu-runner --features capture (debug)",
        "toolu_binary_sha256": (hashlib.sha256(Path(toolu_bin).read_bytes()).hexdigest()
                                if toolu_bin and Path(toolu_bin).is_file() else None),
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--temp", required=True)
    parser.add_argument("--official-dir", required=True)
    parser.add_argument("--home", default=str(Path.home()))
    parser.add_argument("--ephemeral-exit")
    parser.add_argument("--ephemeral-runner-gone", choices=("true", "false"))
    args = parser.parse_args(argv)
    report = {"platform": args.platform, "provenance": provenance(args), "lanes": observations(args),
              "toolu_only": {"ephemeral": ephemeral(args)}}
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["lanes"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
