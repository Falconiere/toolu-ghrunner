# Matcher fixture provenance

`problem_matcher_tsc.json` and `problem_matcher_eslint.json` are setup-node
`.github/tsc.json` and `.github/eslint-stylish.json` at
49933ea5288caeca8642d1e84afbd3f7d6820020 (JSON indentation normalized).
Sources: https://github.com/actions/setup-node/tree/49933ea5288caeca8642d1e84afbd3f7d6820020/.github

Text captures produced on macOS arm64, Node 26.10.0, 2026-09-26:

- TypeScript 5.9.3: `node node_modules/typescript/bin/tsc --pretty false --noEmit --skipLibCheck src/example.ts`.
  Source: `const value: string = 42;` followed by newline. Exit 2.
- ESLint 8.57.1: `node node_modules/eslint/bin/eslint.js --no-eslintrc --env node --rule no-unused-vars:warn --rule no-undef:error --format stylish src/example.js`.
  Source: `var unused = 1;` then `console.log(missing);`, each newline terminated. Exit 1.

The absolute temporary checkout prefix in ESLint output is replaced by
`@WORKSPACE@`, and the final empty line is omitted. Diagnostic messages,
spacing, ordering and ranges are intact.
Replay substitutes its actual workspace and runs real Bash `cat` on these captures.
This is production replay evidence, not live toolu GitHub UI/reference parity.
The acquisition fixture is the existing sanitized #68 capture
`crates/execution/tests/incoming_contexts_matrix_0.json`. Replay preserves selected
step UUIDs/context names and map tokens, replacing only scripts/action path and
clearing unrelated defaults. No mocked service response is used.
