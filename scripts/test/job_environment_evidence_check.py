#!/usr/bin/env python3
"""Check issue 69 fixture provenance; optionally validate real GitHub run evidence."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / 'crates/execution/tests/job_environment_evidence.json'
REPO = 'Falconiere/toolu-ghrunner'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def api(path):
    command = ['gh', 'api', f'repos/{REPO}/{path}']
    if path.endswith('/logs'):
        # Capture ANSI-bearing logs for assertions; never print them to a terminal.
        command.append('--allow-escape-sequences')
    try:
        return subprocess.check_output(command)
    except subprocess.CalledProcessError as error:
        raise SystemExit(f'gh api {path} failed (exit {error.returncode})') from None
    except OSError as error:
        raise SystemExit(f'cannot run gh api {path}: {error}') from None


def check_files(evidence):
    source = evidence['source_capture']
    assert evidence['reference_source_repository'] == 'actions/runner'
    assert source['repository'] == REPO
    original = json.loads((ROOT / 'crates/execution/tests/incoming_contexts_evidence.json').read_text())
    filename = Path(source['path']).name
    assert source['sha256'] == original['captures'][filename]['sha256']
    assert digest((ROOT / source['path']).read_bytes()) == source['sha256']
    assert source['run_id'] == original['captures'][filename]['run_id']
    assert source['revision'] == original['captures'][filename]['revision']
    job = json.loads((ROOT / source['path']).read_text())
    assert job['environmentVariables'] == [], 'source capture unexpectedly changed'
    assert evidence['layers_are_transformed_probe_data'] is True
    for path, expected in evidence['files'].items():
        assert digest((ROOT / path).read_bytes()) == expected, f'{path}: hash mismatch'
    layers = json.loads((ROOT / 'crates/execution/tests/job_environment_layers.json').read_text())
    assert len(layers) == 2 and all(layer['type'] == 2 for layer in layers)
    maps = [{entry['Key']['lit']: entry['Value'] for entry in layer['map']} for layer in layers]
    assert maps[0]['SHARED'] == {'type': 0, 'lit': 'workflow'}
    assert maps[1]['SHARED'] == {'type': 0, 'lit': 'job'}
    assert maps[1]['PRIOR'] == {'type': 3, 'expr': 'env.SHARED'}
    assert maps[0]['MULTILINE']['lit'] == 'first\nsecond\n'
    coverage = (ROOT / 'docs/test-coverage.md').read_text()
    for scenario in ['69-S1', '69-S2', '69-S3', '69-S4']:
        assert scenario in coverage, f'{scenario}: missing coverage row'
    for path, names in evidence['tests'].items():
        text = (ROOT / path).read_text()
        for name in names:
            assert f'fn {name}(' in text, f'{path}: missing test {name}'
    print('Issue 69 acquisition provenance, transformed layers, action/workflow hashes and test map verified.')


def check_run(record, evidence, lane):
    assert record, f'{lane}: unverified; no recorded run'
    run = json.loads(api(f"actions/runs/{record['id']}"))
    assert run['head_sha'] == record['revision'], f'{lane}: revision mismatch'
    assert run['status'] == 'completed' and run['conclusion'] == 'success', f'{lane}: run not successful'
    jobs = json.loads(api(f"actions/runs/{record['id']}/jobs?per_page=100"))['jobs']
    parity = [job for job in jobs if job['name'].startswith('environment (')]
    assert len(parity) == 2, f'{lane}: expected two environment jobs'
    required = set(evidence['required_steps'])
    for job in parity:
        passed = {step['name'] for step in job['steps'] if step['conclusion'] == 'success'}
        assert required <= passed, f"{lane}/{job['name']}: missing assertions {required - passed}"
        assert job['runner_name'] in record['runners'], f'{lane}: runner identity mismatch'
    for path, expected in evidence['files'].items():
        if not path.startswith('.github/'):
            continue
        content = json.loads(api(f"contents/{path}?ref={record['revision']}"))
        assert digest(base64.b64decode(content['content'])) == expected, f'{lane}/{path}: revision hash mismatch'
    for job in parity:
        job_logs = api(f"actions/jobs/{job['id']}/logs").decode()
        assert 'env69-secret:***' in job_logs, f"{lane}/{job['name']}: masked secret marker missing"
    print(f"{lane}: {run['html_url']} verified (runner versions require separate provenance).")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--live', action='store_true', help='Require recorded toolu and pinned-reference evidence')
    parser.add_argument('--hosted', action='store_true', help='Verify hosted probes, without claiming pinned parity')
    args = parser.parse_args()
    evidence = json.loads(EVIDENCE.read_text())
    check_files(evidence)
    if args.hosted:
        check_run(evidence['runs']['hosted'], evidence, 'hosted')
    if args.live:
        for lane in ['toolu', 'pinned_reference']:
            check_run(evidence['runs'][lane], evidence, lane)
        assert evidence['reference_binary_revision'] == evidence['reference_source'], 'Pinned binary provenance unverified'


if __name__ == '__main__':
    main()
