//! Docker step support. Note: container steps are spawned by the Docker daemon,
//! not as child processes here, so the per-job cgroup join used for native steps
//! does not apply — containers must be constrained via `--cgroup-parent` at
//! create time (out of scope for this enforcement fix).

/// Parse `docker://image:tag` into `(image, tag)`. Defaults tag to "latest".
pub fn parse_docker_uses(uses: &str) -> (&str, &str) {
  let without_prefix = uses.strip_prefix("docker://").unwrap_or(uses);

  if let Some(colon_pos) = without_prefix.rfind(':') {
    let after_colon = without_prefix.get(colon_pos + 1..).unwrap_or_default();
    if after_colon.contains('/') {
      (without_prefix, "latest")
    } else {
      (
        without_prefix.get(..colon_pos).unwrap_or(without_prefix),
        after_colon,
      )
    }
  } else {
    (without_prefix, "latest")
  }
}
