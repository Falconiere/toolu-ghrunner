use std::time::Duration;

use protocol::toolu_pair::PairPoll;

use super::super::prompts::{ByteKeys, PromptEnd};
use super::{arch, countdown, display_uri, machine_report, next_interval, run_press_enter};

#[test]
fn slow_down_adds_five_seconds_and_nothing_shortens_the_interval() {
  let five = Duration::from_secs(5);
  assert_eq!(
    next_interval(five, &PairPoll::SlowDown),
    Duration::from_secs(10)
  );
  for poll in [
    PairPoll::Pending,
    PairPoll::Denied,
    PairPoll::Expired,
    PairPoll::Other("x".to_owned()),
  ] {
    assert_eq!(
      next_interval(Duration::from_secs(10), &poll),
      Duration::from_secs(10)
    );
  }
}

#[test]
fn the_countdown_rounds_up_to_the_second() {
  assert_eq!(countdown(Duration::from_secs(900)), "15:00");
  assert_eq!(countdown(Duration::from_millis(892_100)), "14:53");
  assert_eq!(countdown(Duration::ZERO), "00:00");
}

#[test]
fn rust_arches_map_to_the_contract_spelling() {
  assert_eq!(arch("x86_64"), "x64");
  assert_eq!(arch("aarch64"), "arm64");
  assert_eq!(arch("riscv64"), "riscv64");
}

#[test]
fn the_uri_shows_without_its_scheme() {
  assert_eq!(
    display_uri("https://console.toolu.sh/pair"),
    "console.toolu.sh/pair"
  );
  assert_eq!(display_uri("http://127.0.0.1:8/pair"), "127.0.0.1:8/pair");
}

#[test]
fn the_report_carries_this_box_and_the_product_version() {
  let report = machine_report(&["gpu".to_owned()], 8, Some(4096));
  assert_eq!(report.platform, std::env::consts::OS);
  assert_eq!((report.vcpu, report.memory_mb), (Some(8), Some(4096)));
  assert_eq!(report.labels, ["gpu"]);
  assert_eq!(report.runner_version, env!("CARGO_PKG_VERSION"));
  assert!(!report.hostname.is_empty());
}

#[test]
fn press_enter_waits_for_enter_and_ctrl_c_cancels() {
  let mut out = Vec::new();
  run_press_enter(&mut ByteKeys::new(b"x \r"), &mut out, "Press enter").unwrap();
  assert!(String::from_utf8_lossy(&out).contains("Press enter"));
  let mut out = Vec::new();
  assert!(matches!(
    run_press_enter(&mut ByteKeys::new(b"\x03"), &mut out, "Press enter"),
    Err(PromptEnd::Cancelled)
  ));
}
