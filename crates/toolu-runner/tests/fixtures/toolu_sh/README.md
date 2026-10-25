# toolu_sh/

toolu.sh `/runner/*` response bodies for the #206 poll-agent tests
(`tests/poll_loop_test.rs`, `crates/wire/tests/toolu_jobs_wire_test.rs`) and the
#207 pairing tests (`tests/setup_toolu_test.rs`, `tests/toolu_registration_test.rs`).

| File | Provenance | Body |
| --- | --- | --- |
| `invalid_token_401.json` | **live capture**, `dev-console.toolu.sh` 2026-10-24 (provenance inside) | `401 {"error":"invalid or revoked token"}`: unknown or revoked host token |
| `missing_bearer_401.json` | **live capture**, `dev-console.toolu.sh` 2026-10-24 (provenance inside) | `401 {"error":"missing bearer token"}` |
| `pair_200.json` | **live capture**, `dev-console.toolu.sh` 2026-10-25; device code replaced by a synthetic one | `POST /runner/pair` 200: device + user code, `verification_uri(_complete)`, `expires_in` 900, `interval` 5 |
| `pair_token_pending_400.json` | **live capture**, 2026-10-25 | `400 {"error":"authorization_pending"}` |
| `pair_token_slow_down_400.json` | **live capture**, 2026-10-25 (second poll inside `interval`) | `400 {"error":"slow_down"}` |
| `pair_token_expired_400.json` | **live capture**, 2026-10-25 (unknown device code) | `400 {"error":"expired_token"}` |
| `pair_reserved_label_400.json` | **live capture**, 2026-10-25 (`labels: ["toolu-gpu"]`) | `400` naming the reserved `toolu-` label |
| `enroll_invalid_401.json` | **live capture**, 2026-10-25 (unknown `tlu_e_` token) | `401 {"error":"invalid_enroll_token"}` |
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

`GET`, `PATCH` and `DELETE /runner/host` with an unknown host token answered
the same `401 {"error":"invalid or revoked token"}` as `invalid_token_401.json`
on 2026-10-25, so that capture backs them too. The approved `pair/token` 200,
`enroll` 200 and `/runner/host` 200 bodies are contract-derived: they need an
org admin's console approval or a minted enrollment token (#207 S4, unverified).
