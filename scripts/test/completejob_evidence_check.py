#!/usr/bin/env python3
"""Issue-88 CompleteJob evidence: capture sanitization and live-lane checks.

Modes:
  --sanitize RAW_DIR  turn raw acquired bodies into committed fixtures
  --local             verify the committed captures (sanitized, hashed, shaped)
  --record RUN_ID     record a completejob-88.yml run's GitHub API results
  (default)           every required lane passed or recorded unverified
  --strict            every required lane passed (non-gating)
  --docs              documentation mentions every #88 acceptance row
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from uuid import NAMESPACE_URL, uuid5

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / 'crates/execution/tests'
EVIDENCE = ROOT / 'crates/listener/tests/completejob_88_evidence.json'
REPO = 'Falconiere/toolu-ghrunner'
PIN = 'cab9d1c3901e45c7705889c4f88284fdd93f4ae5'
CAPTURES = {'url-toolu': 'env', 'secret-toolu': 'secret', 'steps-toolu': 'steps'}
LANES = ('toolu', 'reference')
ALLOWED_UNVERIFIED = {'macos', 'ghes'}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def fixture(kind):
    return FIXTURES / f'completejob_88_{kind}_message.json'


def placeholder(kind, path):
    return str(uuid5(NAMESPACE_URL, f'toolu-ghrunner/88/{kind}/{path}'))


def secret_paths(job):
    """Every captured credential value, keyed by its JSON path."""
    found = {}
    for key, variable in job.get('variables', {}).items():
        if variable.get('isSecret'):
            found[f'variables/{key}/value'] = ('variables', key)
    for index, _ in enumerate(job.get('mask', [])):
        found[f'mask/{index}/value'] = ('mask', index)
    for index, endpoint in enumerate(job.get('resources', {}).get('endpoints', [])):
        for key in endpoint.get('authorization', {}).get('parameters', {}):
            found[f'resources/endpoints/{index}/authorization/parameters/{key}'] = (
                'endpoint', index, key)
    return found


def read_secret(job, where):
    if where[0] == 'variables':
        return job['variables'][where[1]]['value']
    if where[0] == 'mask':
        return job['mask'][where[1]]['value']
    return job['resources']['endpoints'][where[1]]['authorization']['parameters'][where[2]]


def write_secret(job, where, value):
    if where[0] == 'variables':
        job['variables'][where[1]]['value'] = value
    elif where[0] == 'mask':
        job['mask'][where[1]]['value'] = value
    else:
        job['resources']['endpoints'][where[1]]['authorization']['parameters'][where[2]] = value


def sanitize(raw_dir):
    evidence = json.loads(EVIDENCE.read_text()) if EVIDENCE.exists() else {}
    captures = {}
    for raw in sorted(Path(raw_dir).glob('*.json')):
        data = raw.read_bytes()
        job = json.loads(data)
        kind = CAPTURES.get(job.get('jobDisplayName'))
        if kind is None:
            continue
        originals = {}
        for path, where in secret_paths(job).items():
            original = read_secret(job, where)
            if original:
                originals[original] = placeholder(kind, path)
                write_secret(job, where, placeholder(kind, path))
        text = json.dumps(job, indent=2, ensure_ascii=False) + '\n'
        # A credential echoed anywhere else in the body is replaced as well.
        for original, value in sorted(originals.items(), key=lambda item: -len(item[0])):
            text = text.replace(original, value)
        fixture(kind).write_text(text)
        captures[fixture(kind).relative_to(ROOT).as_posix()] = {
            'job': job.get('jobDisplayName'),
            'raw_sha256': sha256(data),
            'sha256': sha256(fixture(kind).read_bytes()),
            'secret_fields_replaced': len(originals),
        }
    missing = set(CAPTURES.values()) - {
        CAPTURES[c['job']] for c in captures.values()}
    if missing:
        sys.exit(f'missing captures: {sorted(missing)}')
    evidence.setdefault('issue', 88)
    evidence.setdefault('official_runner_commit', PIN)
    evidence['captures'] = captures
    EVIDENCE.write_text(json.dumps(evidence, indent=2) + '\n')
    print(f'sanitized {len(captures)} captures')


def check_local():
    evidence = json.loads(EVIDENCE.read_text())
    captures = evidence['captures']
    assert len(captures) == len(CAPTURES), captures
    for rel, meta in captures.items():
        path = ROOT / rel
        assert sha256(path.read_bytes()) == meta['sha256'], f'{rel} hash drift'
        job = json.loads(path.read_text())
        kind = CAPTURES[meta['job']]
        for json_path, where in secret_paths(job).items():
            assert read_secret(job, where) == placeholder(kind, json_path), (
                f'{rel}: {json_path} is not its UUIDv5 placeholder')
        assert job.get('billingOwnerId'), f'{rel}: billingOwnerId missing'
    env = json.loads(fixture('env').read_text())
    url = env['actionsEnvironment']['url']
    assert url['type'] == 3 and 'steps.deploy.outputs.url' in url['expr'], url
    secret = json.loads(fixture('secret').read_text())
    assert secret['actionsEnvironment']['name'].startswith('completejob-88-secret')
    steps = json.loads(fixture('steps').read_text())
    assert 'actionsEnvironment' not in steps or steps['actionsEnvironment'] is None
    print('local captures ok')


def api(path):
    return json.loads(subprocess.check_output(['gh', 'api', path]))


def record(run_id):
    evidence = json.loads(EVIDENCE.read_text())
    run = api(f'repos/{REPO}/actions/runs/{run_id}')
    jobs = api(f'repos/{REPO}/actions/runs/{run_id}/jobs?per_page=100')['jobs']
    lanes = {}
    for lane in LANES:
        lane_jobs = {j['name'].rsplit('-', 1)[0]: j for j in jobs if j['name'].endswith(f'-{lane}')}
        result = {'runner': None, 'jobs': {}}
        for name, job in sorted(lane_jobs.items()):
            result['runner'] = job.get('runner_name')
            steps = [{'number': s['number'], 'name': s['name'], 'conclusion': s['conclusion']}
                     for s in job['steps']]
            annotations = api(f'repos/{REPO}/check-runs/{job["id"]}/annotations')
            log = subprocess.check_output(
                ['gh', 'api', '--allow-escape-sequences', f'repos/{REPO}/actions/jobs/{job["id"]}/logs'], text=True)
            result['jobs'][name] = {
                'id': job['id'], 'conclusion': job['conclusion'], 'steps': steps,
                'log_lines': len(log.splitlines()),
                'annotations': [{'level': a['annotation_level'], 'message': a['message']}
                                for a in annotations]}
        for kind in ('', 'secret-'):
            env = f'completejob-88-{kind}{lane}'
            deployments = api(f'repos/{REPO}/deployments?environment={env}&sha={run["head_sha"]}')
            urls = []
            for deployment in deployments:
                statuses = api(f'repos/{REPO}/deployments/{deployment["id"]}/statuses')
                urls.extend(s.get('environment_url', '') for s in statuses if s['state'] == 'success')
            result['jobs'].setdefault('url' if not kind else 'secret', {})['environment_urls'] = urls
        lanes[lane] = result
    evidence['runs'] = {'id': int(run_id), 'head_sha': run['head_sha'], 'lanes': lanes}
    EVIDENCE.write_text(json.dumps(evidence, indent=2) + '\n')
    print(f'recorded run {run_id}')


def lane_status(evidence, lane):
    """passed / failed with reasons / unverified, judged against the reference lane."""
    unverified = evidence.get('unverified', {})
    if lane in unverified:
        return 'unverified', [unverified[lane]]
    runs = evidence.get('runs')
    if not runs:
        return 'failed', ['no recorded run and no recorded unverified reason']
    jobs = runs['lanes'][lane]['jobs']
    run_id = runs['id']
    problems = []
    expected_url = f'https://toolu-88.example/{lane}/{run_id}'
    if expected_url not in jobs.get('url', {}).get('environment_urls', []):
        problems.append(f'url job environment_url != {expected_url}')
    if any(jobs.get('secret', {}).get('environment_urls', [])):
        problems.append('secret job published an environment_url')
    if not any('may contain secret' in a['message'] for a in jobs.get('secret', {}).get('annotations', [])):
        problems.append('secret job lacks the skip-url warning annotation')
    for name in ('url', 'secret', 'steps'):
        steps = jobs.get(name, {}).get('steps', [])
        last = max(steps, key=lambda s: s['number'], default=None)
        if not last or last['name'] != 'Complete job':
            problems.append(f'{name}: Complete job is not the highest-numbered row')
    for name in ('url', 'secret', 'steps'):
        if not jobs.get(name, {}).get('log_lines'):
            problems.append(f'{name}: job log did not load')
    for name in ('url', 'secret', 'steps'):
        if jobs.get(name, {}).get('conclusion') != 'success':
            problems.append(f'{name}: conclusion {jobs.get(name, {}).get("conclusion")}')
    return ('passed' if not problems else 'failed'), problems


def check_lanes(strict):
    evidence = json.loads(EVIDENCE.read_text())
    ok = True
    for lane in LANES:
        status, notes = lane_status(evidence, lane)
        print(f'{lane}: {status.upper()} {"; ".join(notes)}')
        if status == 'failed' or (strict and status != 'passed'):
            ok = False
    for lane, reason in evidence.get('unverified', {}).items():
        if lane not in LANES:
            print(f'{lane}: UNVERIFIED {reason}')
            if lane not in ALLOWED_UNVERIFIED:
                ok = False
    passed = all(lane_status(evidence, lane)[0] == 'passed' for lane in LANES)
    print('AC-6: PASSED' if passed else 'AC-6: UNVERIFIED')
    return ok


def check_docs():
    coverage = (ROOT / 'docs/test-coverage.md').read_text()
    section = coverage.split('# CompleteJob payload parity (#88)', 1)
    assert len(section) == 2, 'test-coverage.md lacks the #88 section'
    for ac in range(1, 9):
        assert f'AC-{ac}' in section[1], f'#88 section lacks AC-{ac}'
    assert 'Those belong to step reporting (#88)' not in coverage, 'stale #99 Complete job note'
    for readme, module in [
        ('crates/execution/src/execution/job_runner/README.md', 'complete_step.rs'),
        ('crates/execution/src/execution/README.md', 'environment_url.rs'),
    ]:
        assert module in (ROOT / readme).read_text(), f'{readme} lacks {module}'
    assert 'outage_override' in (ROOT / 'AGENTS.md').read_text(), 'AGENTS.md lacks outage_override'
    print('docs ok')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--sanitize')
    parser.add_argument('--local', action='store_true')
    parser.add_argument('--record')
    parser.add_argument('--strict', action='store_true')
    parser.add_argument('--docs', action='store_true')
    args = parser.parse_args()
    if args.sanitize:
        sanitize(args.sanitize)
    elif args.local:
        check_local()
    elif args.record:
        record(args.record)
    elif args.docs:
        check_docs()
    else:
        sys.exit(0 if check_lanes(args.strict) else 1)


if __name__ == '__main__':
    main()
