# Authenticated, host-aware action downloads — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** Resolve remote action archives through the job's GitHub service before downloading them.

## Problem

Remote `uses:` refs currently become anonymous `api.github.com` tarball requests in both prefetch and step execution. Private/internal actions fail, GHES and GitHub Connect cannot be routed by the server, and anonymous rate limits can stop public actions. Issue #76 requires the official runner's service resolution, scoped download token, host-aware older-server fallback, secret-safe errors, and production-path evidence.

## Non-Goals

1. Windows zip archives and immutable OCI action packages remain outside epic #67.
2. Proxy/CA configuration (#78/#94/#95) and the shared reference-runner harness (#103) retain their issue ownership. This change must not claim those lanes pass without running them.
3. Local `./` actions remain workspace reads; no remote lookup is made for them.

## Architecture

The acquired job message is the authority for `system.github.launch_endpoint`, `SystemVssConnection` URL and `AccessToken`, `system.github.token`, and `github.api_url`. For Run Service jobs, POST deduplicated `{action,version,path}` references to `{launch_endpoint}/actions/build/{planId}/jobs/{jobId}/runnerresolve/actions` with the connection token. This is the pinned official runner's Launch request. For legacy GHES jobs, use the job server's `ResolveActionDownloadInfoAsync` resource (`27d7f831-88c1-4719-8ca1-6a061dad90eb`, API 6.0-preview.1, `jobId` query) through V1 connection-data discovery, using the connection token. Validate each response entry's URL, resolved revision, and action identity before it reaches the fetcher. Use the returned tar URL and scoped authentication token. When that token is absent, use the job's `system.github.token`, as upstream does; register both with the shared masker before any request or diagnostic.

If the resolution capability is absent or the endpoint explicitly reports unsupported, use the job token at the acquired `github.api_url` (or a derived `github.server_url`: `api.github.com` for github.com, `{server_url}/api/v3` for GHES). Resolve the ref to a commit SHA through that API before downloading `/tarball/{sha}`; if the revision lookup fails, fail without publishing a cache entry. This keeps moved tags/branches distinct. Never infer that a GitHub Connect archive must be on the GHES host. Resolution 401/403/422/429, malformed 2xx, and service/transport failure are errors, not fallback signals. Only transient transport/5xx retry with a bounded budget and `Retry-After` where applicable; cancellation interrupts delays. Preserve distinct auth and availability failures without placing token or response body in user-facing errors.

Use a per-job resolver shared by prefetch and top-level/nested step-time fetches. Cache identity must include the server-resolved SHA (and effective host) so a moved tag/branch cannot reuse old content. Single-flight keys use the same identity. Existing staging, watermark, tar-slip guard and failed-entry eviction remain the commit/retry boundary. Match upstream archive authentication: Basic `x-access-token:<token>` by default, Bearer only when the acquired `UseBearerTokenForCodeload` feature is true and the archive URL is a codeload host/path. Mask the raw token and its Basic encoding. Fetch the archive with at most five redirects. Send Authorization only to the archive URL's exact origin; a cross-origin redirect is followed over HTTPS without Authorization, even when the target is a GitHub codeload service. Reject downgrade to HTTP (except loopback in tests), URL userinfo, and redirect loops. A signed archive URL with no bearer remains usable. The resolver response is not logged wholesale.

## Interfaces / Schema

- Acquired Run Service request: `POST /actions/build/{planId}/jobs/{jobId}/runnerresolve/actions`, bearer `SystemVssConnection.authorization.parameters.AccessToken`, JSON `{"actions":[{"action":"owner/repo","version":"ref","path":"subdir-or-null"}]}`. Response `actions` maps `owner/repo@ref` keys to `{name,resolved_name,resolved_sha,tar_url,version,authentication:{token,expires_at}}` in the exact snake_case spelling defined by the pinned `LaunchContracts.cs`. Preserve that shape in the sanitized service fixture.
- Legacy GHES request: POST the V1 action-download-info resource with `{actions:[{nameWithOwner,ref,path}]}` and `jobId`; decode `ActionDownloadInfoCollection.Actions` into the same internal shape.
- Internal `ActionDownloadInfo`: requested ref, resolved owner/repo and SHA, parsed archive URL, optional scoped token. It is job-private and debug output must redact the token.
- `ActionFetcher::ensure` takes that typed info plus a cancellation token; it does not construct a tarball URL.
- Fallback revision lookup: `{api_url}/repos/{owner}/{repo}/commits/{ref}`, then archive `{api_url}/repos/{owner}/{repo}/tarball/{resolved_sha}` with path segments encoded. `api_url` comes from acquired context or a validated server URL, never a fixed github.com constant for GHES.

## Failure modes and edge cases

- Absent launch endpoint on Run Service or explicitly unsupported resolution: bounded host-aware fallback. Missing job token or unknown API host: fail before download.
- Resolution auth denial, missing/private action, policy denial, 422, 429, malformed/partial response: report a sanitized failure; no fallback that could bypass service policy. Retry only transient failures; honor `Retry-After` and cancellation.
- Expired scoped token or tarball 401/403: resolve once more to refresh token, then fail clearly if still denied. Tarball 404 is not automatically an auth retry. Cross-origin redirect must discard bearer; same-origin redirect may retain it. Reject non-HTTP(S), URL userinfo and HTTPS downgrade.
- Interrupted/corrupt extraction leaves no completed cache entry. Concurrent callers for one resolved revision share one attempt; changed revision has a different cache key. Validate owner, repo, ref and subpath against traversal and URL injection.
- GHES and Connect response URLs may point to a host other than the GHES API host; use the returned archive URL as authoritative after URL validation.

## Acceptance criteria

- **AC-1:** Given an acquired Run Service job and public/private/internal refs by SHA, tag, branch and subdirectory, prefetch and step-time paths request server download info and execute the intended resolved revision with its returned URL/token; a local action makes no network request. Covers 76-S1.
- **AC-2:** Given a GHES action and a GitHub Connect action, the legacy server resolution's returned host and token are used. Given an older server without resolution support, fallback uses the job token and the acquired host's API URL. Covers 76-S2.
- **AC-3:** Given redirects, expiry and 401/403/404/429/5xx responses, credentials stay off cross-origin requests and all log sinks; retry/fallback remains bounded, respects retry guidance, and does not turn authorization denial into fallback. Covers 76-S3.
- **AC-4:** Given cancellation, corrupt archives, concurrent fetches and moved refs, no partial cache becomes complete, retry succeeds safely, and cache identity matches the resolved revision without traversal. Covers 76-S4.
- **AC-5:** The exact issue-owned test, user docs and `docs/test-coverage.md` map the original criteria and 76-S1–S4 to real inputs, observable results, runnable checks, platform/host applicability and evidence links. `./tools/check.sh all` passes.

## Acceptance evidence

| AC | Representative real input and observable result | Boundary/failure input | Runnable check and lane |
| --- | --- | --- | --- |
| AC-1 | Sanitized captured acquisition with launch endpoint and actual `actions/checkout` revision; server response capture retains wire structure and a disposable token; production `run_job` resolves, downloads and executes the action, with exact SHA/content assertion. | Captured `./` local action; ref/subpath variants from pinned real repository revisions. | `cargo test -p execution --test action_downloads_test`; GitHub.com live workflow and pinned reference runner at the same action revisions. |
| AC-2 | GHES acquisition and download-info response including Connect's cross-host tar URL; downloaded content matches returned SHA. | Recorded unsupported legacy endpoint; fallback URL equals that host's `github.api_url` and carries only job token. | `cargo test -p execution --test action_downloads_test`; live GHES/Connect job when credentials and host exist. Missing host is unverified. |
| AC-3 | Actual HTTP redirect and failure responses from a controlled local service, with the real request/response and log sink path; received Authorization headers and durable output show no leak. | Cross-origin redirect, expired token, 401/403/404/429/5xx, `Retry-After`, cancel during wait. | `cargo test -p execution --test action_downloads_test`; real GitHub service/UI confirmation for backend-owned outcomes. |
| AC-4 | Real tar.gz archive of a pinned action revision extracted through the production fetcher; SHA-derived cache key and file hashes match. | Truncated/corrupt tar stream, aborted fetch, concurrent same ref, moved ref, traversal archive. | `cargo test -p execution --test action_downloads_test`; `./tools/check.sh all`. |
| AC-5 | Issue-specific fixture provenance and evidence table includes Linux/macOS and GitHub.com/GHES status, pinned reference comparison, exact commands and links. | A skipped or unavailable lane is recorded as unverified. | `python3 scripts/test/action_download_evidence_check.py`, `./tools/check.sh all`, and live lane commands recorded in `docs/test-coverage.md`. |

## Documentation impact

Update `README.md` action-download behavior and `docs/test-coverage.md` with issue #76's matrix and verified versus unverified lanes. Update the actions module README for any new file. Keep the spec and plan under ignored `docs/toolu/` per repository convention; the user-facing evidence map is tracked.

## Open Questions

None blocking design. The Launch JSON spelling is pinned by upstream `LaunchContracts.cs`; the GHES resource GUID/query/version are pinned by upstream `TaskHttpClientBase.cs`, while its actual path template must be read from the server's connection-data response. GHES/Connect live access and the pinned reference lane are acceptance evidence requirements; if unavailable, report them unverified rather than calling the issue ready.
