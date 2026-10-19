use ivy_host_runtime::{config::{configuration_path, instance_config}, health::check_health, node_runtime::NodeRuntime, IvyJson, IvyResult};

async fn run() -> IvyResult<IvyJson> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let path = configuration_path(&args, std::env::var_os("IVY_INSTANCE_CONFIG"))?;
    let node = NodeRuntime::discover(None).await?;
    let config = instance_config(path, &node).await?;
    let endpoint = std::env::var("IVY_HIVE_HEALTH_URL").ok();
    let health = check_health(&config, endpoint.as_deref(), true).await?;
    Ok(IvyJson::object([("ok", true.into()), ("buildId", health.required("buildId")?.clone()), ("generation", health.required("generation")?.clone())]))
}

#[tokio::main]
async fn main() {
    let (value, code) = match run().await {
        Ok(value) => (value, 0),
        Err(error) => (IvyJson::object([("ok", false.into()), ("code", IvyJson::String(error.code_js()))]), 1),
    };
    println!("{}", ivy_contracts::json::stringify(&value, 1024).expect("bounded health CLI result"));
    std::process::exit(code);
}
