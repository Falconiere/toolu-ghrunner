#!/usr/bin/env python3
"""Verify issue 71's sanitized acquisition and recorded live runs."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess
from uuid import NAMESPACE_URL, uuid5


ROOT = Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / 'crates/execution/tests/defaults_run_evidence.json'
REPO = 'Falconiere/toolu-ghrunner'
PIN = 'cab9d1c3901e45c7705889c4f88284fdd93f4ae5'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def api(path):
    return json.loads(subprocess.check_output(['gh', 'api', f'repos/{REPO}/{path}']))


def literal_mapping(token):
    assert token['type'] == 2
    result = {}
    for entry in token['map']:
        key = entry['Key']
        assert key['type'] == 0
        result[key['lit']] = entry['Value']
    return result


def check_capture(evidence):
    record = evidence['capture']
    path = ROOT / record['fixture']
    assert digest(path) == record['fixture_sha256']
    job = json.loads(path.read_text())
    assert job['jobId'] == record['job_id']
    assert len(job['defaults']) == record['defaults_layers'] == 2
    assert len(job['steps']) == record['steps'] == 8
    github = {entry['k']: entry['v'] for entry in job['contextData']['github']['d']}
    assert str(github['run_id']) == str(record['run_id'])
    assert github['sha'] == record['revision']
    layers = [literal_mapping(token)['run'] for token in job['defaults']]
    assert [
        {key: value['lit'] for key, value in literal_mapping(layer).items()}
        for layer in layers
    ] == [
        {'shell': 'bash', 'working-directory': 'defaults-71-workflow'},
        {'shell': 'sh', 'working-directory': 'defaults-71-job'},
    ]
    assert all(step['id'] for step in job['steps'])
    fields = {}
    for name, item in job['variables'].items():
        if item.get('isSecret'):
            fields[f'variables/{name}/value'] = item['value']
    for index, item in enumerate(job['mask']):
        fields[f'mask/{index}/value'] = item['value']
    for index, endpoint in enumerate(job['resources']['endpoints']):
        for name, value in endpoint.get('authorization', {}).get('parameters', {}).items():
            fields[f'resources/endpoints/{index}/authorization/parameters/{name}'] = value
    assert len(fields) == record['secret_fields_replaced']
    for field, actual in fields.items():
        expected = str(uuid5(NAMESPACE_URL, f'toolu-ghrunner/71/defaults_run_job.json/{field}'))
        assert actual == expected, field
    serialized = path.read_text()
    assert 'ghs_' not in serialized and 'ghp_' not in serialized
    for name, expected in evidence['workflow_sha256'].items():
        assert digest(ROOT / name) == expected, name
    assert evidence['reference_source'] == PIN
    print('Captured defaults, provenance, hashes, and synthetic credentials verified.')


def check_run(record, evidence, lane):
    assert record is not None, f'{lane} live run is unverified'
    run = api(f"actions/runs/{record['id']}")
    assert run['head_sha'] == record['revision']
    assert run['status'] == 'completed' and run['conclusion'] == 'success', lane
    jobs = api(f"actions/runs/{record['id']}/jobs?per_page=100")
    assert jobs['total_count'] == 2, lane
    matching = [job for job in jobs['jobs'] if job['name'] == record['job_name']]
    assert len(matching) == 1, lane
    job = matching[0]
    assert job['runner_name'] == record['runner'], lane
    assert job['conclusion'] == 'success', lane
    required = {
        'Prepare distinct directories', 'Assert job defaults',
        'Assert explicit step overrides', 'Assert directory with spaces',
        'Assert absolute directory', 'Assert composite scope',
        'Assert top-level scope restored',
    }
    passed = {step['name'] for step in job['steps'] if step['conclusion'] == 'success'}
    assert required <= passed, (lane, required - passed)
    for name, expected in evidence['workflow_sha256'].items():
        remote = api(f"contents/{name}?ref={record['revision']}")
        actual = hashlib.sha256(base64.b64decode(remote['content'])).hexdigest()
        assert actual == expected, (lane, name)
    print(f"{lane}: {run['html_url']} passed all issue 71 steps.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--live', action='store_true')
    parser.add_argument('--lane', choices=['host-fallback', 'linux-container'])
    args = parser.parse_args()
    evidence = json.loads(EVIDENCE.read_text())
    check_capture(evidence)
    if args.lane:
        raise SystemExit(f'{args.lane} remains an external unverified lane')
    if args.live:
        capture = evidence['capture']
        for name, expected in evidence['capture_workflow_sha256'].items():
            remote = api(f"contents/{name}?ref={capture['revision']}")
            actual = hashlib.sha256(base64.b64decode(remote['content'])).hexdigest()
            assert actual == expected, ('capture', name)
        for lane in ('toolu', 'reference'):
            check_run(evidence['runs'][lane], evidence, lane)


if __name__ == '__main__':
    main()
