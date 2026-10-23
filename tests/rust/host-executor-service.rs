#[allow(dead_code)]
#[path = "host-service-fixture.rs"]
mod service_fixture;
use service_fixture::{ServiceFixture, fixture_node, host_configuration};

#[path = "host-executor-service-cases.rs"]
mod executor_service_cases;

#[path = "host-bootstrap-worker-entry-cases.rs"]
mod worker_entry_cases;

#[path = "host-bootstrap-automatic-launch-cases.rs"]
mod automatic_launch_cases;

#[path = "host-executor-native-constructor-cases.rs"]
mod native_constructor_cases;

#[path = "host-backup-storage-cases.rs"]
mod backup_storage_cases;
