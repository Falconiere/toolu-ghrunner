# VPS host runbook

How to take one OVHcloud box from "does not exist" to a `vps_hosts` row
serving real jobs. This repo publishes two things a box needs — the
`toolu-daemon` binary and the Docker-capable container image
([`docs/container-image.md`](container-image.md)) — but ordering the
hardware, installing sysbox, wiring the Cloudflare Tunnel and bringing
the row online in toolu.sh's console are all manual, by design (the
design spec's non-goal 3: this delivers a runbook, not a script).

Follow the sections in order — each one depends on the last, and step 1
is the longest lead time in the whole project by a wide margin. Where a
step can fail silently (the box looks fine but is not serving), that
failure mode is called out explicitly.

## 1. Order the OVHcloud box

**Start this first and do everything else while it is in flight.** It
is, by far, the longest pole here.

OVHcloud's **US** entity is a completely separate account, API endpoint
and KYC process from the EU one — `api.us.ovhcloud.com`, not
`api.ovh.com`. An EU account, EU API credentials or an EU support
relationship carry over to nothing on the US side; if SidegigLLC's
capacity needs to land on US iron, the order has to originate on the US
account from the start.

**KYC (identity/order verification) is the real lead time, and it is
measured in days, not hours.** After the order is placed, the account
can sit in "Awaiting documents" for a stretch with the delivery blocked
the entire time — OVH gives roughly 48 hours to upload an ID before the
window lapses, and the "hours" figure in OVH's own marketing for
bare-metal delivery only describes provisioning *after* KYC clears, not
the wait to clear it. Budget for this taking multiple days end to end,
and check the order status page rather than assuming silence means
progress.

Once KYC clears, bare-metal provisioning itself really is on the order
of hours. Do not order blind, though — **section 2 has to be verified
before you place the order**, because the OS choice is fixed at order
time and a reinstall later costs you the provisioning wait again.

## 2. OS choice and `sysbox-runc` — verify before you order, not after

The Docker-capable image tag ([`docs/container-image.md`](container-image.md#the-docker-capable-variant))
only isolates a job's inner `dockerd` when the job container runs under
`sysbox-runc`. Sysbox needs the kernel to support **either shiftfs or
idmapped mounts**, and not every distro OVH images ships with either
one available out of the box. This is an **order-time** decision:
confirm it against OVH's actual image list for the SKU you are
ordering, before you place the order, not after the box arrives.

What to check, concretely:

- **Kernel version** the candidate image ships: `uname -r` needs to be
  recent enough for native idmapped mounts (Linux ≥ 5.19 is the safe
  floor; older kernels need the `shiftfs` DKMS module installable
  instead, which is a second thing that can silently fail to build
  against a vendor-patched kernel).
- **Distro**: Ubuntu LTS (22.04/24.04) is sysbox's primary tested
  target. If OVH's catalog for the chosen SKU only offers something
  else, that is a reason to pick a different SKU or a different image,
  not to proceed and hope.
- Whether the image's storage/filesystem setup (OVH's default
  partitioning, any RAID/LVM layer it adds) is one shiftfs or idmapped
  mounts actually work over — a nonstandard root filesystem is a
  documented sysbox failure mode.

If none of the images for a SKU satisfy this, that is a reason to
change the SKU or the box, decided **before** ordering — see section 1.

## 3. Registering sysbox as a Docker runtime

Installing the `sysbox-ce` (or `sysbox-ee`) package is not the end of
this step. Docker only knows how to run a container under a runtime it
has been told about, and `--runtime=sysbox-runc` (the daemon's own
default, `TOOLU_DAEMON_RUNTIME`) fails at container-**create** time,
not at Docker-daemon-start time, if the runtime was never registered.

After installing sysbox, add it to `/etc/docker/daemon.json`:

```json
{
  "runtimes": {
    "sysbox-runc": {
      "path": "/usr/bin/sysbox-runc"
    }
  }
}
```

(Merge this into any existing `daemon.json` — do not overwrite other
keys already there.) Then:

```sh
systemctl restart docker
docker info --format '{{json .Runtimes}}'   # must include "sysbox-runc"
```

That second command is exactly what
[`.github/workflows/daemon-live.yml`](../.github/workflows/daemon-live.yml)
checks on the repo's own CI runner — it currently only warns when the
runtime is missing there, because nothing that workflow runs today
needs it, but a production VPS host is not that case: an unregistered
runtime here means the daemon's `docker create` call itself fails, for
every job, from the moment the box goes live — see the failure-mode
note in section 8 for what that looks like from the outside.

## 4. Installing the daemon

Download the release tarball from this repo's GitHub Releases
(`Falconiere/toolu-ghrunner`, tag `vX.Y.Z`):

```sh
curl -fsSLO https://github.com/Falconiere/toolu-ghrunner/releases/download/vX.Y.Z/toolu-daemon-linux-amd64.tar.gz
curl -fsSLO https://github.com/Falconiere/toolu-ghrunner/releases/download/vX.Y.Z/SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS
tar -xzf toolu-daemon-linux-amd64.tar.gz   # ./toolu-daemon, ./scripts/toolu-daemon.service
```

Use `arm64` in place of `amd64` if the box is arm. Then:

```sh
# system user — the service does not run as root, but does need docker.sock
useradd --system --no-create-home --shell /usr/sbin/nologin toolu-daemon
usermod -aG docker toolu-daemon

install -m 0755 toolu-daemon /usr/local/bin/toolu-daemon
install -d -m 0755 /var/lib/toolu-daemon        # WorkingDirectory=
install -d -m 0755 /etc/toolu-daemon
install -m 0644 scripts/toolu-daemon.service /etc/systemd/system/toolu-daemon.service
```

The unit ([`scripts/toolu-daemon.service`](../scripts/toolu-daemon.service))
takes **no CLI flags** — every setting comes from
`/etc/toolu-daemon/toolu-daemon.env`, which is a **required**
`EnvironmentFile=` (no leading `-`): a missing file is a startup error,
not a silent empty-env launch. Every variable it reads
(`crates/daemon/src/config.rs`):

| Variable | Required? | Meaning | Sizing note |
| --- | --- | --- | --- |
| `TOOLU_DAEMON_TOKEN_FILE` | yes | Path to the bearer token file. | See section 5 — write it *before* you enable the host in the console. |
| `TOOLU_DAEMON_VCPU` | yes | Whole-vCPU **budget** for concurrently *started* jobs. | **Not a job count.** It is the resource ceiling the gate admits *starts* against — leave headroom below the box's raw core count for the host OS, `dockerd`, and every job's own inner `dockerd` under the Docker-capable image. |
| `TOOLU_DAEMON_MEMORY_MB` | yes | Memory **budget**, in MB, for concurrently started jobs. | Same rule as vCPU — this is not "memory per job", it is the whole pool jobs draw from. Size it below the box's physical RAM, with headroom for the same overhead. |
| `TOOLU_DAEMON_IMAGE` | yes | The image ref pre-pulled at startup and kept resident, and the **only** image this host will run. | Must be the `-docker` tag ([`docs/container-image.md`](container-image.md#the-docker-capable-variant)), pinned by digest, and must match `vps_hosts.image_ref` for this row exactly. A mismatch is a total outage for this host, not a wrong-image build: every create is refused with a `503` naming both values and an `ERROR` in the journal — see section 8. |
| `TOOLU_DAEMON_BIND` | no (default `127.0.0.1:8080`) | Listen address. | Leave it loopback — the Cloudflare Tunnel (section 6) is what makes it reachable, not a public bind. |
| `TOOLU_DAEMON_QUEUE_MAX` | no (default `32`) | Hard queue-depth ceiling — the *only* source of a 429. | A 429 permanently fails a customer's job (no requeue path exists anywhere upstream), so this is a last-resort ceiling, not a normal operating limit. Raise it rather than relying on it to shed load. |
| `TOOLU_DAEMON_RUNTIME` | no (default `sysbox-runc`) | Docker runtime job containers are created under. | Leave the default. Changing it away from `sysbox-runc` removes the isolation the Docker-capable image's whole design depends on — see [`docs/container-image.md`](container-image.md#the-docker-capable-variant). |

A concrete sizing example: the largest Linux tag toolu currently offers
is `toolu-ubuntu-16vcpu-32gb`. The gate only checks that *one* job fits
the remaining budget (`vps_hosts.slots` is display-only, never
enforced) — a box has to have `TOOLU_DAEMON_VCPU`/`_MEMORY_MB` at least
that large just to ever admit that tag's jobs at all, and enough beyond
it, per job you actually want running concurrently, plus headroom for
host/`dockerd` overhead as above.

Bring it up:

```sh
systemctl daemon-reload
systemctl enable --now toolu-daemon
journalctl -u toolu-daemon -f
```

`toolu-daemon is serving` is the line that means startup finished
(logged with `bind`, `image`, `vcpu`, `memory_mb`) — see section 8 for
what precedes it and what can keep it from ever appearing.

## 5. The token file

`TOOLU_DAEMON_TOKEN_FILE` points at a plain text file the daemon
re-reads **fresh on every request** — there is no in-memory cache to
invalidate and no restart involved in anything below.

- **Line 1** is the current token, accepted always.
- **Line 2**, if present, is the *previous* token, accepted only during
  a rotation window.

Both lines are trimmed of surrounding whitespace; either can be blank
in the sense of "absent", but line 1 must not be empty or the daemon
treats the file as unusable.

**Bootstrap** (bringing a brand-new host up for the first time):
`compute.hosts.create` in the console mints the token and shows it to
the operator **exactly once** — write that value as line 1 of the file
now, on the box, with tight permissions:

```sh
install -d -m 0750 -o toolu-daemon -g toolu-daemon /etc/toolu-daemon
printf '%s\n' '<token from the console>' > /etc/toolu-daemon/token
chmod 0600 /etc/toolu-daemon/token
chown toolu-daemon:toolu-daemon /etc/toolu-daemon/token
```

Point `TOOLU_DAEMON_TOKEN_FILE` at that path in the env file from
section 4. The row the console just created is born **DISABLED** —
this is expected, and section 7 is what flips it on.

**Rotation** (`compute.hosts.rotateToken` in the console, any time
after): the console mints and shows a new token once. Write the **new**
token *above* the old one — new token becomes line 1, the token that
used to be line 1 becomes line 2:

```sh
{ printf '%s\n' '<new token>'; printf '%s\n' '<old token>'; } > /etc/toolu-daemon/token.new
mv /etc/toolu-daemon/token.new /etc/toolu-daemon/token
```

No restart, no outage — the daemon now accepts both. Once you are
satisfied the rotation has taken (the console side stores only the new
token, so this is really just "give it a moment"), drop line 2 so the
old token stops being accepted:

```sh
head -n1 /etc/toolu-daemon/token > /etc/toolu-daemon/token.new
mv /etc/toolu-daemon/token.new /etc/toolu-daemon/token
```

## 6. The Cloudflare Tunnel

`scripts/lib/tunnel.sh` in the toolu.sh repo only manages three fixed
local-development hostnames (`local.toolu.sh`, `local-console.toolu.sh`,
`local-api.toolu.sh`) — it has no concept of a VPS host and does not
cover this at all. A new box's tunnel is set up by hand, once, per box.

1. **Create the tunnel** (on the box, or from wherever `cloudflared` is
   available and authenticated against the right Cloudflare account):

   ```sh
   cloudflared tunnel create <box-name>
   ```

   This mints a tunnel id and a credentials JSON file.

2. **Point a hostname at it** — pick a subdomain that names the box,
   e.g. `vps-1.toolu.sh`:

   ```sh
   cloudflared tunnel route dns <box-name> vps-1.toolu.sh
   ```

3. **Ingress config** (`/etc/cloudflared/config.yml` on the box), one
   rule forwarding the hostname to the daemon's loopback bind, plus the
   required catch-all:

   ```yaml
   tunnel: <box-name>
   credentials-file: /etc/cloudflared/<tunnel-id>.json
   ingress:
     - hostname: vps-1.toolu.sh
       service: http://localhost:8080
     - service: http_status:404
   ```

4. **Run it as a service**:

   ```sh
   cloudflared service install
   systemctl enable --now cloudflared
   ```

**Critically: the hostname must carry no Access policy, no WAF rule and
no rate limit.** This is not a hardening suggestion to skip for
convenience — it is a correctness requirement. Cloudflare Access, if
attached to this hostname, redirects an unauthenticated request (which
is *every* request the daemon's own bearer-auth scheme sends — it is
not a browser session) to an HTML login page. The toolu.sh client reads
that response and classifies it `unavailable` — indistinguishable, on
that client, from the daemon actually being down — which triggers the
same fallback-plus-cooldown path a real outage would. Every job
scheduled on this host then fails or falls through silently, nothing in
the daemon's own logs shows anything wrong (the request never reaches
it), and there is no error message anywhere that says "Access policy".
A WAF rule or rate limit produces the same shape of problem: a
Cloudflare-generated 429 looks, on the wire, identical to the daemon's
own capacity 429 — the *only* thing that tells them apart is the
`sh-toolu-daemon: 1` header the daemon stamps on every response it
produces itself (Cloudflare's own error pages never carry it).

Sanity-check before moving on:

```sh
curl -i https://vps-1.toolu.sh/v1/jobs -X DELETE
```

With nothing else in the way this should come back `401` (missing
bearer token — every route requires one) **and carry
`sh-toolu-daemon: 1`**. If you get an HTML body, a redirect, or a 429
with no `sh-toolu-daemon` header, something in front of the tunnel
(Access, WAF, rate limiting) is still attached to the hostname — fix
that before touching the console.

## 7. Bringing it online in the console

Bringing a host up is deliberately **two separate calls**, not one,
because of a chicken-and-egg problem: the bearer token has to exist
before it can be written to the box, but it has to be persisted in the
database before the console can show it to the operator at all.

1. **`compute.hosts.create`** mints the token, **persists the row**,
   shows the plaintext token **once**, and lands the row **disabled**.
   It probes nothing — the box could not possibly hold this token yet,
   so a probe here would just 401 every time and no host could ever be
   created at all.
2. The operator writes that token to the box (section 5) and completes
   sections 3–6 so the daemon is actually serving behind the tunnel.
3. **`compute.hosts.enable`** probes the daemon with the **stored**
   token — a real request against `DELETE /v1/jobs?jobId=…` with a
   sentinel job id that matches nothing (the daemon's reap route is
   404-tolerant and always answers `204`, so this is a genuine
   round-trip through auth with no side effect) — and only flips
   `enabled: true` on a box that actually answered correctly. Nothing
   enters the placement pool without having passed that probe.

If `enable` fails, the console surfaces the daemon's own rejection
reason where there is one; the likely causes, in the order worth
checking:

- The token file on the box does not yet contain the token this call
  is probing with (wrong path, wrong permissions the daemon process
  cannot read, or it was simply never written — section 5).
- The tunnel is not up yet, or the hostname in `baseUrl` does not match
  what section 6 actually wired (section 6).
- An Access policy, WAF rule or rate limit is still attached to the
  hostname (section 6) — this one is the most likely to *look* like a
  timeout or an auth failure rather than naming itself.
- The daemon process itself never finished starting (section 4/8) —
  `journalctl -u toolu-daemon` on the box is the source of truth here,
  not the console.

## 8. Verifying

**On the box:**

```sh
journalctl -u toolu-daemon -f
```

Startup is sequential and load-bearing
([`crates/daemon/README.md`](../crates/daemon/README.md)): config load
→ connect to Docker → adopt whatever containers already exist → pull
`TOOLU_DAEMON_IMAGE` until it is resident → **only then** bind the
listener. `toolu-daemon is serving` is the line that means all of that
finished; anything short of it means the host is not accepting jobs yet
(the process either crash-loops under `Restart=always` or is still
retrying a pull) even though `systemctl status` may show it as
"active".

```sh
docker info --format '{{json .Runtimes}}'   # must include "sysbox-runc"
```

**In the console:** `compute.hosts.enable` succeeding (section 7) is
the confirmation that the daemon is reachable through the tunnel and
authenticates correctly. It does **not** confirm the whole path
end-to-end — the probe never creates a container.

**What CI actually proves, and what it does not.**
[`daemon-live.yml`](../.github/workflows/daemon-live.yml), dispatched
by hand against this repo's own self-hosted CI runner, exercises the
daemon's own Docker calls for real: create/start, destroy, reap,
budget accounting and startup adoption after a restart. It does
**not** exercise sysbox isolation — its own comment says so plainly,
and today it only *warns*, never fails, when `sysbox-runc` is not
registered on whatever runner it happens to execute on. **No automated
test anywhere proves AC-9**: a job container the daemon started
actually running `docker build` and `docker run --rm hello-world` to
completion under `sysbox-runc`, with no host Docker socket mounted.
The first real proof of that, on any given box, is dispatching a real
job to it.

Do that once, deliberately, before trusting the box with customer
traffic: point a test repo's `runs-on:` at this host's tag, with a step
that runs `docker build` and `docker run --rm hello-world`, and read
the job log — not just its pass/fail, the actual output — for a real
`hello-world` run rather than `docker: command not found` (no Docker in
the image at all — wrong image tag) or the job hanging silently to its
six-hour deadline (the isolation is broken and the inner `dockerd`
never came up; see [`docs/container-image.md`](container-image.md#the-docker-capable-variant)'s
note that a `dockerd` failure never fails the *job*, only the steps
that actually touch Docker).

**Failure modes that look the same from the outside, worth telling
apart:**

- **Image not resident yet** and **`sysbox-runc` not registered as a
  Docker runtime** (section 3 skipped or `daemon.json` never updated)
  both surface identically to a client: every `POST /v1/jobs` on this
  host answers `503`, which `vpsDispositionFor` reads as "try another
  host, and cool this one down for five minutes." The daemon's own
  create-path does **not** log the per-request Docker error to
  `journalctl` — that reason only reaches the caller's response body
  and, from there, the customer-visible `runner_instances.error_message`
  row in toolu.sh. If a host is 503-ing every job, check that row's
  text (or query the daemon directly with a real bearer token) rather
  than the box's own journal, which stays quiet about it.
- A **wrong or stale `TOOLU_DAEMON_IMAGE`** (not matching
  `vps_hosts.image_ref`) is a **total outage for this host**, not a
  cosmetic mismatch. The daemon pre-pulls and keeps resident exactly one
  image — this variable — while every create names its own, from
  `vps_hosts.image_ref`. When the two disagree the daemon refuses the
  create outright: `503`, with a body that names both values
  (`image mismatch: this host pins … and cannot run …`), and an
  **`ERROR`-level line in `journalctl -u toolu-daemon`** saying the same.
  That log line is the one create-path failure the box does report about
  itself, precisely because it is the one an operator can fix. Every job
  routed here fails, and `vpsDispositionFor` re-stamps a five-minute
  cooldown on each delivery, so the host also drains. Fix it by making the
  two values equal — `TOOLU_DAEMON_IMAGE` in
  `/etc/toolu-daemon/toolu-daemon.env` and `vps_hosts.image_ref` for this
  row — then `systemctl restart toolu-daemon`.
