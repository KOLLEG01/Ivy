use super::{App, failure};
use ivy_contracts::IvyJson;
use std::sync::Arc;
use tokio::sync::watch;

pub(super) async fn run(
    app: Arc<App>,
    upgraded: hyper::upgrade::OnUpgrade,
    context: IvyJson,
    stopping: watch::Receiver<bool>,
) {
    let result = async {
        let upgraded = upgraded.await.map_err(failure)?;
        app.sockets
            .run(
                hyper_util::rt::TokioIo::new(upgraded),
                app.server.network.peers.clone(),
                context,
                stopping,
            )
            .await
    }
    .await;
    if let Err(error) = result {
        eprintln!("Hive WebSocket: {error:?}");
    }
}
