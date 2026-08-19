# daemon

The toolu compute daemon: the server half of `vps_hosts`.

toolu.sh's `packages/api/src/providers/vps/client.ts` has always spoken this
HTTP contract; until this crate existed there was nothing on the other end, so
no VPS host had ever booted a job. The daemon runs on a toolu-operated box,
sits behind a Cloudflare Tunnel, and turns `POST /v1/jobs` into one
sysbox-isolated container running `toolu-runner boot`.

Three properties are forced by that client and must not be traded away:

- **Admit and queue, never refuse.** The Worker delivers `workflow_job.queued`
  exactly once and has no requeue path, so a 429 permanently fails a customer's
  job. `docker create` returns an id immediately and the resource gate governs
  `start`; 429 is reserved for a hard queue-depth ceiling
  (`TOOLU_DAEMON_QUEUE_MAX`). `TOOLU_DAEMON_VCPU`/`TOOLU_DAEMON_MEMORY_MB` are a
  **budget**, not a job-count ceiling: an admitted job that does not fit right
  now simply waits, tracked, until another job finishes and releases its share
  — there is no dimensionless "slots" number anywhere in the gate.
- **Never pull inside a request.** The client's timeout is 10 seconds and a
  timeout is terminal-with-cooldown on its side. The pinned image is pulled at
  startup and on a timer; a request that arrives before it is resident gets a
  fast 503.
- **Creates survive a client disconnect.** The client aborts with
  `AbortSignal.timeout`, which cancels the HTTP handler mid-await. Orchestration
  runs on a detached task keyed by the job id so Docker work is never left half
  done, and the id is recorded — in an in-memory **tombstone registry**
  (`crate::docker::registry`) — before create, so `DELETE /v1/jobs?jobId=` can
  always address an in-flight create, even one Docker does not know about yet.
  The gate bookkeeping that follows the create — record the container, or give
  the admission back — runs on that same detached task for the same reason: a
  disconnect that cancelled it left a queue slot consumed by a job nothing
  tracked, and `TOOLU_DAEMON_QUEUE_MAX` of those wedged the host at 429 with no
  path back.
- **One host, one image.** `TOOLU_DAEMON_IMAGE` is the only image pulled or kept
  resident, and a create naming any other is refused with a 503 that says so, in
  the body and in the journal. A `vps_hosts.image_ref` that has drifted from it
  is a total outage for the host, and it used to look like a slow pull.

State lives in Docker, not in this process: budget is released when a container
exits, and every live job is rebuilt from container labels at startup
(`crate::adopt`) — a routine restart (token rotation, a new binary) resumes
where the previous process left off instead of believing the box is empty.

**Startup order is load-bearing** (`src/main.rs`): load config, connect to
Docker, adopt whatever is already running (seeds the gate before anything can
be admitted against it), pre-pull the pinned image until it is resident, and
only then bind the listener — binding first would answer 503 to the client's
first requests, which reads as a five-minute cooldown on this host. Timers
(the reconcile tick, the periodic image refresh) start once the listener is
up.

**The live suite is not part of the gate.** Every `tests/docker_live*.rs` test
is `#[ignore]`d and runs only from `.github/workflows/daemon-live.yml`, which
is `workflow_dispatch`-only — `cargo test --workspace` never touches them. That
workflow's header lists exactly what is left ungated as a result, and what was
deliberately covered in the pure suite instead. Run it by hand after changing
`src/docker.rs`, `src/reaper.rs` or `src/adopt.rs`.
