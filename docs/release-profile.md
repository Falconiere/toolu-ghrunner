# Release profile: `opt-level = "s"` + fat LTO (#112)

`[profile.release]` is `opt-level = "s"`, `lto = "fat"`, `codegen-units = 1`,
`strip = "symbols"`, `panic = "abort"`. It was adopted only after the hot paths
were measured against the previous `opt-level = 3` + thin LTO.

## Decision rule

Fixed before measuring. The candidate (`"s"` + fat) is adopted if no hot-path
metric is **more than 10% worse** than the baseline (`3` + thin): criterion mean
time for `secret_masker` and `interpolate`, MB/s for `cas_roundtrip`. If it
fails, the fallback `3` + fat is adopted instead and the reason recorded here.
`scripts/test/release_profile_test.sh` pins the shipped profile.

## Results (Linux amd64, 8 cores, shared host, one tree, separate target dirs)

Criterion means (the `bench` profile inherits `release`):

| Bench | 3 + thin (baseline) | "s" + fat (adopted) | Δ | 3 + fat (fallback) |
| --- | --- | --- | --- | --- |
| `secret_masker_mask_100_patterns` | 1.610 µs | 1.743 µs | +8.3% | 1.627 µs |
| `interpolate_no_expression/small_event` | 22.64 ns | 22.44 ns | −0.9% | 21.80 ns |
| `interpolate_no_expression/large_event` | 24.64 ns | 22.03 ns | −10.6% | 21.91 ns |
| `interpolate_github_repository/small_event` | 9.443 µs | 9.962 µs | +5.5% | 8.609 µs |
| `interpolate_github_repository/large_event` | 9.094 µs | 9.822 µs | +8.0% | 9.714 µs |
| `literal_status_condition_success/small_event` | 6.619 ns | 6.454 ns | −2.5% | 5.976 ns |
| `literal_status_condition_success/large_event` | 6.311 ns | 5.921 ns | −6.2% | 5.830 ns |

Accelerated cache, `cargo bench -p cache --bench cas_roundtrip`: a 512 MiB
directory (64 files, random + repetitive; 357 chunks), `CasStore::ingest` then a
BLAKE3-verified `read_range` restore, hash-checked against the source. Median of 3:

| | 3 + thin | "s" + fat | Δ | 3 + fat |
| --- | --- | --- | --- | --- |
| save | 700.8 MB/s | 710.8 MB/s | +1.4% | 721.6 MB/s |
| restore | 1595.8 MB/s | 1545.3 MB/s | −3.2% | 1595.3 MB/s |

Release binaries (stripped) and cold `cargo build --release -p toolu-runner -p daemon` wall time
(empty target dir each run):

| | 3 + thin | "s" + fat | 3 + fat |
| --- | --- | --- | --- |
| `toolu-runner`, Linux amd64 | 13,632,080 B | 9,544,216 B (−30.0%) | 12,874,272 B (−5.6%) |
| `toolu-daemon`, Linux amd64 | 4,664,544 B | 3,550,432 B (−23.9%) | 4,574,408 B (−1.9%) |
| build wall | 205 s | 230 s | 315 s |

macOS arm64 was not re-measured here. The issue's measurements at `2ca6459`
(`toolu-runner` / `toolu-daemon`): 3 + thin 8,575,264 / 3,661,152 B; "s" + fat
6,014,368 B (−29.9%) / 2,874,272 B (−21.5%); 3 + fat 7,829,968 B (−8.7%) /
3,517,824 B (−3.9%). Linux and macOS agree on the ~30% / ~22% shrink.

## Outcome

Every metric is inside the 10% threshold (worst: masker +8.3%,
`interpolate_github_repository` +8.0%; cache within 3.2%), so `"s"` + fat LTO is
adopted. Masking and interpolation run in microseconds per line/expression; the
regression is far below what a job's I/O shows. The masker's criterion
intervals do not overlap (baseline 1.596–1.626 µs, adopted 1.680–1.805 µs), so
its +8.3% is a real shift, which is why the masker is the metric to watch if the profile is
revisited. No per-crate `opt-level = 3` override was needed. The fallback
(`3` + fat, −5.6% only) stays the documented retreat if a later change pushes a
metric past 10%.

## Reproduce

```sh
for cfg in "3 thin" "s fat" "3 fat"; do
  set -- $cfg
  export CARGO_PROFILE_RELEASE_OPT_LEVEL=$1 CARGO_PROFILE_RELEASE_LTO=$2
  export CARGO_TARGET_DIR=/path/to/target-$1-$2          # separate dir per profile
  time cargo build --release --locked -p toolu-runner -p daemon
  cargo bench --locked -p cache --bench cas_roundtrip      # x3, take the median
  cargo bench --locked -p shared --bench secret_masker
  cargo bench --locked -p execution --bench interpolate
done
```

Numbers are from a shared host and vary run to run; re-measure on the same
machine before comparing.
