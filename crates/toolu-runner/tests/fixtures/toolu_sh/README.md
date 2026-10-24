# toolu_sh/

toolu.sh `/runner/*` response bodies for the #206 poll-agent tests
(`tests/poll_loop_test.rs`, `crates/wire/tests/toolu_jobs_wire_test.rs`).

| File | Provenance | Body |
| --- | --- | --- |
| `invalid_token_401.json` | **live capture**, `dev-console.toolu.sh` 2026-10-24 (provenance inside) | `401 {"error":"invalid or revoked token"}`: unknown or revoked host token |
| `missing_bearer_401.json` | **live capture**, `dev-console.toolu.sh` 2026-10-24 (provenance inside) | `401 {"error":"missing bearer token"}` |
| `queued_200.json` | contract-derived: `self-hosted-machines.md` "Claiming a job" example, verbatim | `GET /runner/jobs/queued` for a machine |
| `claim_200.json` | contract-derived: same section's machine claim, verbatim (no `image`) | `POST /runner/jobs/:id/claim` 200 |
| `claim_taken_409.json` | contract-derived: the guide's refusal table + `routes.ts` `claim refused: <reason>` | lost claim race |
| `claim_slots_full_409.json` | contract-derived: as above | machine already holds its slots |
| `heartbeat_200.json` | contract-derived: `routes.ts` `handleHeartbeat` reply shape | renewed lease |
| `ok_200.json` | contract-derived: `routes.ts` `handleDispatched` / `handleComplete` reply | `{ ok: true }` |

Contract-derived bodies are not live: a 200 or 409 needs a paired host token,
which only an org admin's console approval (or enrollment token) can mint.
Tests substitute their own `id`, `jitConfig` and timestamps into these shapes.
A live S5 run (#206 AC-11) replaces them with sanitized captures.
