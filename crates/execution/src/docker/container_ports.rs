//! Docker port-publishing declarations for a job container.

use std::collections::HashMap;

use bollard::models::{ContainerCreateBody, PortBinding};
use shared::RunnerError;

/// Apply container ports and optional host addresses without shell parsing.
pub(crate) fn apply_ports(
  ports: &[String],
  body: &mut ContainerCreateBody,
) -> Result<(), RunnerError> {
  let mut exposed = Vec::new();
  let mut bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
  for port in ports {
    let (mapping, protocol) = port.split_once('/').unwrap_or((port, "tcp"));
    if !matches!(protocol, "tcp" | "udp" | "sctp") {
      return Err(invalid_port());
    }
    let segments: Vec<_> = mapping.split(':').collect();
    let (host_ip, host_port, container_port) = match segments.as_slice() {
      [container] => ("", "", *container),
      [host, container] => ("", *host, *container),
      [ip, host, container] => (*ip, *host, *container),
      _ => return Err(invalid_port()),
    };
    validate_port(container_port)?;
    if !host_port.is_empty() {
      validate_port(host_port)?;
    }
    if !host_ip.is_empty() && host_ip.parse::<std::net::Ipv4Addr>().is_err() {
      return Err(invalid_port());
    }
    let key = format!("{container_port}/{protocol}");
    exposed.push(key.clone());
    bindings
      .entry(key)
      .or_insert_with(|| Some(Vec::new()))
      .as_mut()
      .ok_or_else(invalid_port)?
      .push(PortBinding {
        host_ip: Some(host_ip.to_owned()),
        host_port: Some(host_port.to_owned()),
      });
  }
  body.exposed_ports = Some(exposed);
  body
    .host_config
    .get_or_insert_with(Default::default)
    .port_bindings = Some(bindings);
  Ok(())
}

fn validate_port(port: &str) -> Result<(), RunnerError> {
  if port.parse::<u16>().is_ok_and(|n| n > 0) {
    Ok(())
  } else {
    Err(invalid_port())
  }
}

fn invalid_port() -> RunnerError {
  RunnerError::Docker("container ports must be [IPv4-address:][host-port:]container-port[/tcp|udp|sctp] with ports 1..65535".to_owned())
}
