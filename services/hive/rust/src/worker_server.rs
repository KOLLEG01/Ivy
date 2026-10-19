//! Single storage owner and original worker scheduling, independent of kernel APIs.
use crate::worker_protocol::{FrameReader, Message, Notification, Request, write_frame};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
};

pub type BackupTask = Pin<Box<dyn Future<Output = IvyResult<IvyJson>>>>;
pub type Progress = Arc<dyn Fn() + Send + Sync>;
pub enum Completion {
    Reply(IvyResult<IvyJson>),
    /// Publication itself failed; already emitted notifications remain visible,
    /// but the request chain cannot issue a reply or accept more work.
    Failed(IvyError),
}
pub struct Dispatch {
    pub notifications: Vec<Notification>,
    pub completion: Completion,
}
/// The kernel owns its connection and every action's validation. Backup tasks
/// must use that same connection in yielding batches, as in the original worker.
pub trait WorkerService {
    fn handle(&mut self, payload: IvyJson) -> IvyResult<IvyJson>;
    fn backup(&mut self, payload: IvyJson, progress: Progress) -> IvyResult<BackupTask>;
    fn notifications(&mut self) -> Vec<Notification>;
    /// Existing services keep their action/notification behavior. Kernel-backed
    /// dispatch also preserves the original fallible publication boundary.
    fn dispatch(&mut self, payload: IvyJson) -> Dispatch {
        let close = payload.get("action") == Some(&IvyJson::from("close"));
        let result = self.handle(payload);
        Dispatch {
            notifications: if close { vec![] } else { self.notifications() },
            completion: Completion::Reply(result),
        }
    }
}
struct Backup {
    id: u64,
    task: BackupTask,
    progress: watch::Receiver<u64>,
    progress_open: bool,
    reported: u64,
}
fn progress() -> (Progress, watch::Receiver<u64>) {
    let (sender, receiver) = watch::channel(0);
    let reported = Mutex::new(0_u128);
    let callback = Arc::new(move || {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut last = reported.lock().unwrap();
        if now.saturating_sub(*last) >= 1000 {
            *last = now;
            sender.send_modify(|sequence| *sequence += 1);
        }
    });
    (callback, receiver)
}
async fn finish_backup<W: AsyncWrite + Unpin>(
    writer: &mut W,
    backup: &mut Backup,
) -> io::Result<()> {
    loop {
        tokio::select! {
            biased;
            result = &mut backup.task => {
                pending_progress(writer, backup).await?;
                return write_frame(writer, &Message::Reply { id: backup.id, result }.to_json()).await;
            },
            changed = backup.progress.changed(), if backup.progress_open => {
                backup.progress_open = changed.is_ok();
                pending_progress(writer, backup).await?;
            },
        }
    }
}
async fn pending_progress<W: AsyncWrite + Unpin>(
    writer: &mut W,
    backup: &mut Backup,
) -> io::Result<()> {
    // A completed future can drop the sender before its last update is read.
    // has_changed() then returns Closed even with an unseen value.
    let sequence = *backup.progress.borrow_and_update();
    if sequence != backup.reported {
        backup.reported = sequence;
        write_frame(writer, &Message::Progress(backup.id).to_json()).await?;
    }
    Ok(())
}
enum Event {
    Request(io::Result<Option<IvyJson>>),
    Complete(IvyResult<IvyJson>),
    Progress(bool),
}
/// Read one startup options frame, initialize before Ready, then serialize all
/// ordinary work. Backup remains separately polled; Close waits for its result.
pub async fn serve<R, W, F, S>(reader: R, mut writer: W, initialize: F) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnOnce(IvyJson) -> IvyResult<S>,
    S: WorkerService,
{
    let mut reader = FrameReader::new(reader);
    let Some(options) = reader.next().await? else {
        return Ok(());
    };
    let mut service = match initialize(options) {
        Ok(service) => service,
        Err(error) => {
            write_frame(&mut writer, &Message::Fatal(error).to_json()).await?;
            return Ok(());
        }
    };
    write_frame(&mut writer, &Message::Ready.to_json()).await?;
    let mut backup: Option<Backup> = None;
    loop {
        let event = if let Some(running) = backup.as_mut() {
            tokio::select! {
                biased;
                result = &mut running.task => Event::Complete(result),
                changed = running.progress.changed(), if running.progress_open => Event::Progress(changed.is_ok()),
                value = reader.next() => Event::Request(value),
            }
        } else {
            Event::Request(reader.next().await)
        };
        match event {
            Event::Request(value) => {
                let Some(value) = value? else {
                    return Ok(());
                };
                let request = Request::from_json(value)?;
                write_frame(&mut writer, &Message::Started(request.id).to_json()).await?;
                let action = request.payload.get("action").and_then(|v| v.as_str().ok());
                if action.as_deref() == Some("backup") {
                    if backup.is_some() {
                        write_frame(
                            &mut writer,
                            &Message::Reply {
                                id: request.id,
                                result: Err(IvyError::new(
                                    "limit_exceeded",
                                    "A storage backup is already active.",
                                )),
                            }
                            .to_json(),
                        )
                        .await?;
                    } else {
                        let (progress, receiver) = progress();
                        match service.backup(request.payload, progress) {
                            Ok(task) => {
                                backup = Some(Backup {
                                    id: request.id,
                                    task,
                                    progress: receiver,
                                    progress_open: true,
                                    reported: 0,
                                })
                            }
                            Err(error) => {
                                write_frame(
                                    &mut writer,
                                    &Message::Reply {
                                        id: request.id,
                                        result: Err(error),
                                    }
                                    .to_json(),
                                )
                                .await?
                            }
                        }
                    }
                    continue;
                }
                let close = action.as_deref() == Some("close");
                if close {
                    if let Some(mut running) = backup.take() {
                        finish_backup(&mut writer, &mut running).await?;
                    }
                }
                let dispatch = service.dispatch(request.payload);
                for notification in dispatch.notifications {
                    write_frame(&mut writer, &notification.to_json()).await?;
                }
                let result = match dispatch.completion {
                    Completion::Reply(result) => result,
                    Completion::Failed(error) => return Err(io::Error::other(error)),
                };
                write_frame(
                    &mut writer,
                    &Message::Reply {
                        id: request.id,
                        result,
                    }
                    .to_json(),
                )
                .await?;
                if close {
                    return Ok(());
                }
            }
            Event::Complete(result) => {
                let mut running = backup.take().unwrap();
                pending_progress(&mut writer, &mut running).await?;
                write_frame(
                    &mut writer,
                    &Message::Reply {
                        id: running.id,
                        result,
                    }
                    .to_json(),
                )
                .await?;
            }
            Event::Progress(open) => {
                let running = backup.as_mut().unwrap();
                running.progress_open = open;
                pending_progress(&mut writer, running).await?;
            }
        }
    }
}
