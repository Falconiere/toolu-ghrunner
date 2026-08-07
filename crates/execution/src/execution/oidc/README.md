# oidc/

**What belongs here:** the local OIDC token service backing
`ACTIONS_ID_TOKEN_REQUEST_URL` — either proxying to GitHub's real OIDC
provider or minting a locally-signed JWT, plus the GitHub Actions OIDC claim
shape.

**What does NOT belong here:** deciding whether a job even needs a local
OIDC server vs. forwarding real GitHub URLs — that mode selection lives in
`execution::service_endpoints` / `execution::job_runner`. Bearer validation
is shared via `execution::service_auth`, not reimplemented here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `claims.rs` | `OidcClaims` | JWT claims for GitHub-format OIDC tokens (`repository`/`actor`/`ref`/`sha`/`workflow`/`run_id`/…), plus `OidcConfig`/`OidcMode` (GitHub-proxy vs local-mint) and the job-context inputs used to build them. |
| `server.rs` | `OidcServer` | Local axum server bound to `127.0.0.1:0` serving `/_apis/pipeline/oidc/requestToken`: proxies to GitHub in `GitHub` mode or mints an HS256 JWT locally in `Local` mode. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
