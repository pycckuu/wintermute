//! HealthStatus enum tests.

use wintermute::executor::{ExecutorKind, HealthStatus};

#[test]
fn healthy_variant_is_healthy() {
    let status = HealthStatus::Healthy {
        kind: ExecutorKind::Docker,
        details: "running".to_owned(),
    };
    assert!(status.is_healthy());
}

#[test]
fn degraded_variant_is_not_healthy() {
    let status = HealthStatus::Degraded {
        kind: ExecutorKind::Direct,
        details: "maintenance only".to_owned(),
    };
    assert!(!status.is_healthy());
}

#[test]
fn unavailable_variant_is_not_healthy() {
    let status = HealthStatus::Unavailable {
        kind: ExecutorKind::Docker,
        details: "docker not running".to_owned(),
    };
    assert!(!status.is_healthy());
}
