// Behavioral cases from tests/host-executor-parallel.test.ts.
#[path = "../../packages/host-runtime/rust/src/scheduler.rs"]
mod scheduler;

use scheduler::{Deployment, exclusive_deployment, parallel_deployments};

fn entry<'a>(id: &'a str, instance: &'a str, exclusive: bool) -> Deployment<'a> {
    Deployment {
        deployment_id: id,
        instance_id: instance,
        exclusive,
    }
}

#[test]
fn bounded_lanes_preserve_instance_order() {
    let queue = [
        entry("a", "first", false),
        entry("b", "first", false),
        entry("c", "c", false),
        entry("d", "d", false),
    ];
    assert_eq!(parallel_deployments(&queue, &[], 2), [0, 2]);
    assert_eq!(parallel_deployments(&queue, &queue[..1], 2), [2]);
    assert!(parallel_deployments(&queue, &[queue[0], queue[2]], 2).is_empty());
}

#[test]
fn infrastructure_barrier_cannot_be_starved_or_overtaken() {
    let first = entry("a", "a", false);
    let barrier = entry("infra", "infra", true);
    let last = entry("b", "b", false);
    assert_eq!(parallel_deployments(&[first, barrier, last], &[], 2), [0]);
    assert!(parallel_deployments(&[barrier, last], &[first], 2).is_empty());
    assert_eq!(parallel_deployments(&[barrier, last], &[], 2), [0]);
    assert!(parallel_deployments(&[barrier, last], &[barrier], 2).is_empty());
    // Even a same-instance operation waiting for a lane cannot hide a later barrier.
    assert!(
        parallel_deployments(&[entry("next-a", "a", false), barrier, last], &[first], 2).is_empty()
    );
}

#[test]
fn source_preparation_and_verified_hive_kind_are_exclusive() {
    assert!(exclusive_deployment("preparing", "worker", false));
    assert!(exclusive_deployment("checking", "host-executor", false));
    assert!(exclusive_deployment("checking", "custom-hive", true));
    assert!(!exclusive_deployment("checking", "worker", false));
    let build = entry("build", "build", true);
    assert_eq!(
        parallel_deployments(&[build, entry("next", "next", false)], &[], 2),
        [0]
    );
    assert!(parallel_deployments(&[entry("next", "next", false)], &[build], 2).is_empty());
}
