#!/usr/bin/env python3
"""Verify issue-68 captured contexts and, optionally, real GitHub run evidence."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / 'crates/execution/tests'
REPO = 'Falconiere/toolu-ghrunner'
PIN = 'cab9d1c3901e45c7705889c4f88284fdd93f4ae5'


def read(path):
    return json.loads(path.read_text())


def value(node):
    if not isinstance(node, dict):
        return node
    if 'd' in node:
        return {value(e['k']): value(e['v']) for e in node['d']}
    if 'a' in node:
        return [value(v) for v in node['a']]
    for key in ('s', 'b', 'n'):
        if key in node:
            return node[key]
    return None


def api(path):
    return json.loads(subprocess.check_output(['gh', 'api', f'repos/{REPO}/{path}']))


def check_captures(evidence):
    assert evidence['reference_source'] == PIN
    for name, record in evidence['captures'].items():
        path = FIXTURES / name
        assert hashlib.sha256(path.read_bytes()).hexdigest() == record['sha256'], name
        job = read(path)
        assert job['jobId'] == record['job_id']
        contexts = {key: value(data) for key, data in job['contextData'].items()}
        assert contexts['github']['run_id'] == str(record['run_id'])
        assert contexts['github']['sha'] == record['revision']
        if 'matrix_' in name:
            index = int(name.removesuffix('.json')[-1])
            matrix = contexts['matrix']
            assert matrix['index'] == index
            assert type(matrix['number']) is int and matrix['number'] == (7 if index == 0 else 0)
            assert matrix['flag'] is (index == 0)
            assert matrix['tag'] == ('alpha' if index == 0 else 'beta')
            assert contexts['inputs'] == {'who': 'world', 'count': '3', 'enabled': False}
            assert contexts['needs'] == {'producer': {'outputs': {'artifact': 'artifact-68'}, 'result': 'success'}}
            assert contexts['strategy'] == {'fail-fast': False, 'job-index': index, 'job-total': 2, 'max-parallel': 1}
        else:
            inputs = contexts['inputs']
            assert inputs == {'who': 'called', 'count': 0, 'enabled': True}
            assert type(inputs['count']) is int
        for variable in job.get('variables', {}).values():
            if variable.get('isSecret'):
                assert variable['value'] == '[redacted]'
        for endpoint in job.get('resources', {}).get('endpoints', []):
            assert all(v == '[redacted]' for v in endpoint.get('authorization', {}).get('parameters', {}).values())
    for path, digest in evidence.get('toolu_source_sha256', {}).items():
        assert hashlib.sha256((ROOT / path).read_bytes()).hexdigest() == digest, path
    for path, digest in evidence['workflow_sha256'].items():
        assert hashlib.sha256((ROOT / path).read_bytes()).hexdigest() == digest, path
    print('Captured wire types, values, redaction and workflow revisions verified.')


def check_live(evidence):
    for lane in ('reference', 'toolu'):
        record = evidence['runs'][lane]
        assert isinstance(record, dict), f'{lane} live evidence is still unverified'
        run = api(f"actions/runs/{record['id']}")
        assert run['head_sha'] == record['revision']
        assert run['status'] == 'completed' and run['conclusion'] == 'success', lane
        jobs = api(f"actions/runs/{record['id']}/jobs?per_page=100")
        assert jobs['total_count'] == 4
        for job in jobs['jobs']:
            assert job['conclusion'] == 'success', job['name']
            assert all(step['conclusion'] == 'success' for step in job['steps']), job['name']
            if job['name'] != 'producer':
                assert job['runner_name'] == record['runner'], job['runner_name']
                required = ({'Assert typed workflow_call inputs', 'Assert typed condition ran'}
                            if job['name'] == 'call / inputs' else
                            {'Assert incoming contexts', 'Workflow input passed to action',
                             'Independent sibling action input', 'Assert parent workflow scope restored'})
                assert required <= {step['name'] for step in job['steps']}, job['name']
        names = {job['name'] for job in jobs['jobs']}
        assert names == {'producer', 'call / inputs', 'consumer (alpha, 7, true, 0, context-68-alpha)', 'consumer (beta, 0, false, 1, context-68-beta)'}
        for path, digest in evidence['workflow_sha256'].items():
            remote = api(f"contents/{path}?ref={record['revision']}")
            assert hashlib.sha256(base64.b64decode(remote['content'])).hexdigest() == digest, (lane, path)
        print(f"{lane}: {run['html_url']} passed exact-value assertions at matching workflow/action revisions.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--live', action='store_true')
    args = parser.parse_args()
    evidence = read(FIXTURES / 'incoming_contexts_evidence.json')
    check_captures(evidence)
    if args.live:
        check_live(evidence)


if __name__ == '__main__':
    main()
