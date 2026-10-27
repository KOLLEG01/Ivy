//! Production account owners for the ordinary native service startup.
use super::{
    audio_worker::AudioWorker,
    auth::AuthAccount,
    auth_backend::NativeAuthStore,
    journal_io::JournalIo,
    journal_owner::Journal,
    media::WhatsAppMedia,
    messages::Translator,
    native_media::NativeMediaDependencies,
    runtime::{RuntimeResources, RuntimeSocket},
    socket::WhatsAppSocket,
};
use crate::runtime::WhatsAppServiceParts;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult};
use ivy_host_runtime::{config::atomic_json, portable_native_files::NativeHelperLayout};
use std::{path::PathBuf, sync::Arc};

/// `config` is the account admitted by read_whatsapp_config. Merely opening its
/// owners never starts networking; ChatService starts the socket after binding.
pub async fn open(
    config: Json,
    data_root: PathBuf,
    artifact_root: PathBuf,
    executable: PathBuf,
    translator: Translator,
) -> IvyResult<WhatsAppServiceParts> {
    let root = data_root.clone();
    let (backend, journal) = tokio::task::spawn_blocking(move || -> IvyResult<_> {
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        for name in ["whatsapp", "whatsapp-owner"] {
            directory
                .create(root.join(name))
                .map_err(|_| IvyError::internal())?;
        }
        let backend = Arc::new(NativeAuthStore::new(AuthAccount::acquire(
            &root.join("whatsapp/auth.sqlite"),
            &root.join("whatsapp-owner/executor-lock.sqlite"),
        )?));
        let journal = Journal::new(JournalIo::open(root.join("whatsapp.sqlite"))?);
        Ok((backend, journal))
    })
    .await
    .map_err(|_| IvyError::internal())??;
    let socket = match WhatsAppSocket::open(
        config.clone(),
        journal.clone(),
        backend.clone(),
        data_root.clone(),
    )
    .await
    {
        Ok(socket) => socket,
        Err(error) => {
            let _ = backend.close();
            let _ = journal.close().await;
            return Err(error);
        }
    };
    let dependencies = match NativeMediaDependencies::new(AudioWorker::new(executable)) {
        Ok(dependencies) => Arc::new(dependencies),
        Err(error) => {
            let _ = socket.close().await;
            let _ = journal.close().await;
            return Err(error);
        }
    };
    let media = Arc::new(WhatsAppMedia::new(
        journal.clone(),
        config.clone(),
        data_root.clone(),
        dependencies,
        translator,
    ));
    Ok(WhatsAppServiceParts {
        config,
        journal,
        socket: Arc::new(socket),
        media,
        resources: RuntimeResources {
            status: Arc::new(move |value| {
                let path = data_root.join("whatsapp-status.json");
                Box::pin(async move { atomic_json(path, value).await })
            }),
            retire_account: Arc::new(move || {
                let backend = backend.clone();
                Box::pin(async move { backend.close() })
            }),
        },
        admitted_artifact: artifact_root,
        helper_layout: NativeHelperLayout::Target,
    })
}
