#!/usr/bin/env python3
"""Sanitize a raw acquired job body (from the `capture` feature) into a fixture.

Usage:
  sanitize_capture.py RAW OUT [--namespace NAME]   write a sanitized copy
  sanitize_capture.py --scan PATH                  fail if PATH looks unsanitized

Secret values are collected from every place the job message carries one:
secret `variables`, `mask` hints, endpoint `authorization.parameters`, and any
JWT- or GitHub-token-shaped string. Each distinct value becomes a version-5 UUID
derived from the namespace and the JSON path where it was first seen (never
from the value itself, so the fixture holds no hash of a secret) — except a
value that is an endpoint `authorization.parameters` entry, which becomes the
literal `redacted` (the #99 fixture convention; a placeholder under an
`AccessToken` key reads as a live API key to secret scanners). Every
occurrence of that value, including inside longer strings such as scripts, is
replaced by the same placeholder, so equality relations (a mask hint equal to a
variable) survive. Keys, key order, value types, step UUIDs, `contextName`s
and URLs are untouched. The result is re-scanned and refused (non-zero exit)
if a credential-shaped string or an original secret value remains.
"""

import argparse
import json
import sys
import uuid
from pathlib import Path

TOKEN_PREFIXES = ("ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_")
WORD_CHARS = "_-."


def credential_words(text):
    """Credential-shaped words in `text`: GitHub tokens and three-part JWTs."""
    words = "".join(c if c.isalnum() or c in WORD_CHARS else " " for c in text).split()
    # A sentence-final "." would otherwise give a JWT a fourth dot.
    words = [w.rstrip(".") for w in words]
    return [w for w in words
            if (w.startswith(TOKEN_PREFIXES) and len(w) >= 24)
            or (w.startswith("eyJ") and w.count(".") == 2)]


def walk(node, path="$"):
    """Yield (json_path, string) for every string value in `node`."""
    if isinstance(node, str):
        yield path, node
    elif isinstance(node, dict):
        for key, value in node.items():
            yield from walk(value, f"{path}.{key}")
    elif isinstance(node, list):
        for index, value in enumerate(node):
            yield from walk(value, f"{path}[{index}]")


def collect_secrets(message):
    """Ordered {secret_value: first_json_path} for every secret the body carries."""
    found, per_path = {}, {}

    def add(value, path):
        # Two distinct secrets first seen in the same string must not share a
        # placeholder, so the key carries an occurrence index for that path.
        if isinstance(value, str) and value and value not in found:
            index = per_path.get(path, 0)
            per_path[path] = index + 1
            found[value] = path if index == 0 else f"{path}#{index}"

    for name, variable in (message.get("variables") or {}).items():
        if isinstance(variable, dict) and variable.get("isSecret"):
            add(variable.get("value"), f"$.variables.{name}")
    for index, hint in enumerate(message.get("mask") or []):
        if isinstance(hint, dict):
            add(hint.get("value"), f"$.mask[{index}]")
    endpoints = (message.get("resources") or {}).get("endpoints") or []
    for index, endpoint in enumerate(endpoints):
        params = ((endpoint or {}).get("authorization") or {}).get("parameters") or {}
        for key, value in params.items():
            add(value, f"$.resources.endpoints[{index}].authorization.{key}")
    for path, text in walk(message):
        for word in credential_words(text):
            add(word, path)
    return found


def placeholder(namespace, path):
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"toolu-capture:{namespace}:{path}"))


def replace_all(node, mapping):
    """Copy of `node` with every secret substring replaced (longest first)."""
    if isinstance(node, str):
        for secret in sorted(mapping, key=len, reverse=True):
            if secret in node:
                node = node.replace(secret, mapping[secret])
        return node
    if isinstance(node, dict):
        return {key: replace_all(value, mapping) for key, value in node.items()}
    if isinstance(node, list):
        return [replace_all(value, mapping) for value in node]
    return node


def endpoint_secrets(message):
    endpoints = (message.get("resources") or {}).get("endpoints") or []
    return {value for endpoint in endpoints
            for value in (((endpoint or {}).get("authorization") or {}).get("parameters") or {}).values()
            if isinstance(value, str) and value}


def sanitize(message, namespace):
    """Return (sanitized_message, number_of_distinct_secrets)."""
    secrets = collect_secrets(message)
    redacted = endpoint_secrets(message)
    mapping = {value: REDACTED if value in redacted else placeholder(namespace, path)
               for value, path in secrets.items()}
    clean = replace_all(message, mapping)
    problems = scan(clean, originals=secrets)
    if problems:
        raise ValueError("sanitization incomplete: " + "; ".join(problems))
    return clean, len(mapping)


REDACTED = "redacted"


def is_placeholder(value):
    try:
        parsed = uuid.UUID(value)
    except (ValueError, TypeError, AttributeError):
        return False
    return parsed.version == 5 and str(parsed) == value


def scan(message, originals=()):
    """Problems that make `message` unsafe to commit (empty list = clean)."""
    problems = []
    for name, variable in (message.get("variables") or {}).items():
        if isinstance(variable, dict) and variable.get("isSecret") and variable.get("value"):
            if not (is_placeholder(variable["value"]) or variable["value"] == REDACTED):
                problems.append(f"secret variable {name} is not a placeholder")
    for index, hint in enumerate(message.get("mask") or []):
        if (isinstance(hint, dict) and hint.get("value")
                and not (is_placeholder(hint["value"]) or hint["value"] == REDACTED)):
            problems.append(f"mask[{index}] is not a placeholder")
    endpoints = (message.get("resources") or {}).get("endpoints") or []
    for index, endpoint in enumerate(endpoints):
        params = ((endpoint or {}).get("authorization") or {}).get("parameters") or {}
        for key, value in params.items():
            # Older fixtures (#70) carry placeholders here; this tool writes `redacted`.
            if value and not (value == REDACTED or is_placeholder(value)):
                problems.append(f"endpoint[{index}] {key} is not redacted")
    for path, text in walk(message):
        if credential_words(text):
            problems.append(f"credential-shaped value at {path}")
        problems.extend(f"original secret survives at {path}" for secret in originals
                        if secret and secret in text)
    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("raw", nargs="?", type=Path)
    parser.add_argument("out", nargs="?", type=Path)
    parser.add_argument("--namespace", default="compat-103")
    parser.add_argument("--scan", type=Path, metavar="PATH")
    args = parser.parse_args(argv)
    if args.scan:
        problems = scan(json.loads(args.scan.read_text()))
        for problem in problems:
            print(f"{args.scan}: {problem}", file=sys.stderr)
        if problems:
            return 1
        print(f"{args.scan}: clean")
        return 0
    if not args.raw or not args.out:
        parser.error("RAW and OUT are required unless --scan is given")
    message = json.loads(args.raw.read_text())
    try:
        clean, count = sanitize(message, args.namespace)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    args.out.write_text(json.dumps(clean, indent=2, ensure_ascii=False) + "\n")
    print(f"{args.out}: replaced {count} distinct secret values")
    return 0


if __name__ == "__main__":
    sys.exit(main())
