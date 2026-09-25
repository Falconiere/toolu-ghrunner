#!/usr/bin/env python3
"""Verify issue 102's captured replay provenance and optional live parity runs."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]
REPO = 'Falconiere/toolu-ghrunner'
# Upstream source revision only; live lane verification is recorded in evidence['runs'].
PIN = 'cab9d1c3901e45c7705889c4f88284fdd93f4ae5'
LANES = ('toolu-macos', 'reference-macos', 'reference-linux')
REQUIRED_STEPS = {
    'Install the committed local actions', 'Expressions and field scopes',
    'Check expression values', 'Continue on error inside composite',
    'Check continued outcome', 'Uncontinued inner failure with cleanup',
    'Check failure cleanup and reporting', 'Hard nested action error with cleanup',
    'Check hard error cleanup', 'Repeated nested action scope',
    'Check nested scope', 'File commands and Node state',
    'Check exported env and PATH',
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def api(path):
    return json.loads(subprocess.check_output(['gh', 'api', f'repos/{REPO}/{path}']))


def check_capture(evidence):
    record = evidence['capture']
    path = ROOT / record['path']
    assert digest(path) == record['sha256'], 'captured job hash changed'
    original = json.loads((ROOT / 'crates/execution/tests/incoming_contexts_evidence.json').read_text())
    pinned = original['captures'][path.name]
    assert record['sha256'] == pinned['sha256'], 'captured job differs from issue 68 provenance'
    job = json.loads(path.read_text())
    assert job['jobId'] == record['job_id'] == pinned['job_id'], 'captured job ID changed'
    assert record['run_id'] == pinned['run_id'], 'captured run ID changed'
    parent = [step for step in job['steps'] if step.get('contextName') == '__self']
    assert len(parent) == 1 and parent[0]['id'] == record['parent_step_id'], 'parent wire ID changed'
    with_values = parent[0]['inputs']['map']
    who = [entry['Value'] for entry in with_values if entry['Key'].get('lit') == 'who']
    assert len(who) == 1 and who[0]['type'] == 3, 'expression-valued input wire type changed'
    assert evidence['reference_source'] == PIN, 'pinned reference source changed'
    for relative, expected in evidence['files_sha256'].items():
        assert digest(ROOT / relative) == expected, f'{relative}: content hash changed'
    print('Captured acquisition, parent ID, expression token, and committed file hashes verified.')


def check_run(lane, record, evidence):
    run = api(f"actions/runs/{record['id']}")
    assert run['head_sha'] == record['revision'], f'{lane}: revision mismatch'
    assert run['status'] == 'completed' and run['conclusion'] == 'success', (
        f"{lane}: {run['status']}/{run['conclusion']}"
    )
    jobs = api(f"actions/runs/{record['id']}/jobs?per_page=100")['jobs']
    matching = [job for job in jobs if job['name'] == f'composite-{lane}']
    assert len(matching) == 1, f'{lane}: expected one matching job'
    job = matching[0]
    assert job['conclusion'] == 'success', f'{lane}: job failed'
    if record.get('runner'):
        assert job['runner_name'] == record['runner'], f'{lane}: runner identity changed'
    passed = {step['name'] for step in job['steps'] if step['conclusion'] == 'success'}
    assert REQUIRED_STEPS <= passed, f'{lane}: missing passing steps {REQUIRED_STEPS - passed}'
    for relative, expected in evidence['files_sha256'].items():
        remote = api(f"contents/{relative}?ref={record['revision']}")
        assert hashlib.sha256(base64.b64decode(remote['content'])).hexdigest() == expected, (
            f'{lane}: remote {relative} differs'
        )
    print(f"{lane}: {run['html_url']} passed the committed assertions.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('evidence', type=Path)
    parser.add_argument('--live', action='store_true', help='Verify recorded runs through GitHub API')
    parser.add_argument('--require-live', action='store_true', help='Fail if any applicable lane is unverified')
    args = parser.parse_args()
    try:
        evidence = json.loads(args.evidence.read_text())
    except (OSError, json.JSONDecodeError) as error:
        parser.error(f'cannot read evidence {args.evidence}: {error}')
    check_capture(evidence)
    if set(evidence['runs']) != set(LANES):
        parser.error('update evidence file: runs keys must match required live lanes')
    for lane in LANES:
        record = evidence['runs'][lane]
        if record is None:
            reason = evidence['unverified'].get(lane)
            if not reason:
                parser.error(f'{lane}: missing unverified reason')
            if args.require_live:
                parser.error(f'{lane}: unverified — {reason}')
            print(f'{lane}: unverified — {reason}')
        elif args.live or args.require_live:
            check_run(lane, record, evidence)


if __name__ == '__main__':
    main()
