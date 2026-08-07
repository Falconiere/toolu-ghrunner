// The house oxlint plugin — the structural rules OXC can enforce directly.
//
// Everything here is a fact about a source file: where it sits, what it is
// named, what it calls. oxlint already has the file open and its AST parsed, so
// enforcing these here rather than in a second bash pass means one enforcement
// surface, one traversal, and an error at the moment the file is written rather
// than at commit time.
//
// What deliberately did NOT move: facts about files oxlint never lints (a
// folder's README, a required wrangler.jsonc, a shadowing lefthook.yaml, banned
// deps in package.json, a secret tracked by git) and the entire Rust stack,
// which oxlint cannot parse. Those stay in scripts/guardrails/.
//
// Wire it up in .oxlintrc.json:
//   "jsPlugins": ["./scripts/guardrails/oxlint-plugin/index.js"],
//   "rules": { "house/folder-tree": "error", … }
import { colocatedTests } from './rules/colocated-tests.js';
import { folderTree } from './rules/folder-tree.js';
import { noBareFetch } from './rules/no-bare-fetch.js';
import { noBarrels } from './rules/no-barrels.js';
import { noHardcodedHex } from './rules/no-hardcoded-hex.js';
import { noModuleScopeDatabase } from './rules/no-module-scope-database.js';

export default {
  meta: { name: 'house' },
  rules: {
    'folder-tree': folderTree,
    'colocated-tests': colocatedTests,
    'no-bare-fetch': noBareFetch,
    'no-barrels': noBarrels,
    'no-hardcoded-hex': noHardcodedHex,
    'no-module-scope-database': noModuleScopeDatabase,
  },
};
