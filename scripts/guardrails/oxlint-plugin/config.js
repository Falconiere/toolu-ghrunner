// Reads guardrails.config.json — the SAME file the bash checks read.
//
// This is what keeps "one declaration" true after the split: oxlint enforces the
// per-file rules and the bash module enforces the repo-level ones, but both take
// their numbers and allowlists from here. A ceiling or an allowlist is edited in
// exactly one place, and validate-templates.sh asserts the linter config agrees
// with it.
//
// Fails CLOSED. A missing or malformed config throws at plugin load, and oxlint
// reports "Failed to load JS plugin" and exits nonzero — it never lints on with
// the rules silently disabled.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const CONFIG_FILE = process.env.GR_CONFIG ?? 'guardrails.config.json';

function load() {
  const path = resolve(process.cwd(), CONFIG_FILE);
  let raw;
  try {
    raw = readFileSync(path, 'utf8');
  } catch (error) {
    throw new Error(
      `guardrails: no config at ${path} — the house oxlint plugin cannot enforce structure without it`,
      { cause: error },
    );
  }
  try {
    return JSON.parse(raw);
  } catch (error) {
    throw new Error(`guardrails: ${path} is not valid JSON`, { cause: error });
  }
}

export const config = load();

/** Path relative to the project root, with OS separators normalised to "/". */
export function relative(filename) {
  const cwd = process.cwd().replaceAll('\\', '/');
  const file = filename.replaceAll('\\', '/');
  return file.startsWith(`${cwd}/`) ? file.slice(cwd.length + 1) : file;
}

/** Is this file under srcRoot?
 *
 * Every rule here is scoped this way, matching the bash checks it replaced —
 * they all walked `find $srcRoot`. Without it the rules reach root-level config
 * and framework directories that are not source: Expo's router lives at `app/`
 * OUTSIDE src/, so an unscoped no-barrels reads its index.tsx routes as barrels,
 * and app.config.ts legitimately carries a literal colour.
 */
export function isUnderSrc(relPath) {
  return relPath.startsWith(`${config.srcRoot}/`);
}

/** Does this path sit inside the project's test directory? */
export function isInTestDir(relPath) {
  const dir = config.testDir;
  return relPath.includes(`/${dir}/`) || relPath.startsWith(`${dir}/`);
}

// Sentinels for the two `**` forms, parked while the single-segment `*` is
// expanded and substituted LAST. Expanding `**` in place emitted a `.*` that the
// `*` pass then found and rewrote into `.[^/]*` — which is how `src/app/**` came
// to mean "exactly one more segment, at least one character" and stopped
// exempting the nested TanStack Router routes it exists for.
//
// They must be plain, printable, non-regex characters that cannot occur in a
// glob. A NUL byte works too, and was what shipped — but it makes git classify
// this file as binary, so `git diff` shows nothing and the review layers that
// read the PR diff cannot see this code at all.
const ANY_DEPTH = '<%d%>'; // `**/` — zero or more leading path segments
const ANY_CHARS = '<%c%>'; // `**`  — anything, path separators included

/** Glob match supporting `*` (one segment) and `**` (any depth). */
export function matchesGlob(relPath, glob) {
  const pattern = glob
    .replaceAll('.', String.raw`\.`)
    .replaceAll('**/', ANY_DEPTH)
    .replaceAll('**', ANY_CHARS)
    .replaceAll('*', '[^/]*')
    .replaceAll(ANY_CHARS, '.*')
    .replaceAll(ANY_DEPTH, '(?:.*/)?');
  return new RegExp(`^${pattern}$`).test(relPath);
}

/** True when any glob in the list matches. */
export function matchesAny(relPath, globs) {
  return (globs ?? []).some((glob) => matchesGlob(relPath, glob));
}
