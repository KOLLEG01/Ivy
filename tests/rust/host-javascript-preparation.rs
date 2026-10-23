#[path = "host-prepare-workflow-cases.rs"]
mod workflow;

#[tokio::main]
async fn main() {
    workflow::emitted_javascript_native_activation_prepares_real_service_and_registers_publication(
    )
    .await;
    workflow::emitted_javascript_preparation_uses_lock_closure_and_reuses_dependencies_for_a_new_build()
        .await;
    workflow::emitted_javascript_inventory_failure_records_outcome_and_releases_cache().await;
}
