use ivy_contracts::{IvyError, IvyResult, json};
use ivy_host_runtime::config::{configuration_path, native_instance_config};
use ivy_phone_bridge::journal::{
    PhoneJournal, PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner,
};
use ivy_phone_bridge::service_runtime::{PhoneService, validate_current_settings};
use tokio::io::AsyncReadExt;

async fn run() -> IvyResult<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() == 1 && args[0] == "--validate-settings" {
        let mut bytes = Vec::new();
        tokio::io::stdin()
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| IvyError::internal())?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            IvyError::new("invalid_arguments", "Phone settings must be UTF-8 JSON.")
        })?;
        validate_current_settings(&json::parse(text, 1_048_576)?)?;
        return Ok(());
    }
    let path = configuration_path(&args, std::env::var_os("IVY_INSTANCE_CONFIG"))?;
    let config = native_instance_config(path).await?;
    if args
        .first()
        .is_some_and(|arg| arg == "--upgrade-voice-storage")
    {
        let source = config.json();
        if source.get("componentId") != Some(&"phone-bridge".into()) {
            return Err(IvyError::new(
                "invalid_arguments",
                "Offline Voice storage requires a PhoneBridge instance.",
            ));
        }
        let text = |name| {
            source
                .required(name)?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        };
        let result = PhoneJournal::upgrade_voice_storage(PhoneJournalOptions {
            data_root: config.data_root().into(),
            owner: PhoneJournalOwner {
                host_id: text("hostId")?,
                service_node_id: text("serviceNodeId")?,
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: source
                .get("settings")
                .and_then(|s| s.get("binding"))
                .and_then(|b| b.get("maxConcurrentCalls"))
                .and_then(ivy_contracts::IvyJson::as_f64)
                .unwrap_or(1.0) as usize,
        })
        .await?;
        println!("{}", json::stringify(&result, 4096)?);
        return Ok(());
    }
    let service = PhoneService::start(config).await?;
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|_| IvyError::internal())?;
        tokio::select! { result=service.wait_closed()=>return result,
        _=tokio::signal::ctrl_c()=>{}, _=term.recv()=>{} }
    }
    #[cfg(not(unix))]
    tokio::select! { result=service.wait_closed()=>return result, _=tokio::signal::ctrl_c()=>{} }
    service.close().wait().await
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!(
            "{}",
            serde_json::json!({"code":error.code,"message":"PhoneBridge failed to start or stop."})
        );
        std::process::exit(1);
    }
}
