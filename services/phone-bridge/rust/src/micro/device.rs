//! Runtime-owned loopback USB/IP server. It installs no driver and performs no
//! attachment command or Desktop lifecycle work.
use super::{
    MicroClose, MicroDiagnostic, MicroInputReceipt, MicroInputResult, MicroUsbSession,
    MicroVoiceObservation, gate::Gate,
};
use socket2::{Domain, Socket, Type};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

struct State {
    sessions: Vec<MicroUsbSession>,
    completions: Vec<(MicroUsbSession, MicroClose)>,
    current: Option<MicroUsbSession>,
    generation: i64,
    closed: bool,
    accepting: Option<MicroClose>,
    heartbeat: Option<MicroClose>,
}
struct Owner {
    gate: Gate,
    state: Mutex<State>,
    stop: CancellationToken,
    port: u16,
    diagnostic: Option<MicroDiagnostic>,
}
#[derive(Clone)]
pub struct MicroUsbDevice(Arc<Owner>);
impl MicroUsbDevice {
    pub fn new(
        port: i32,
        initial_generation: i64,
        diagnostic: Option<MicroDiagnostic>,
    ) -> MicroInputResult<Self> {
        if !(0..=65535).contains(&port) || !(0..=2_097_151).contains(&initial_generation) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Valid loopback port and generation required.",
            )
            .into());
        }
        let _runtime = tokio::runtime::Handle::try_current()?;
        let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            use windows_sys::Win32::Networking::WinSock::{
                SO_EXCLUSIVEADDRUSE, SOCKET_ERROR, SOL_SOCKET, WSAGetLastError, setsockopt,
            };
            let enabled = 1i32;
            // SAFETY: the live socket and four-byte BOOL remain valid for this
            // one setsockopt call, before binding or exposing the listener.
            if unsafe {
                setsockopt(
                    socket.as_raw_socket() as _,
                    SOL_SOCKET,
                    SO_EXCLUSIVEADDRUSE,
                    (&enabled as *const i32).cast(),
                    std::mem::size_of_val(&enabled) as i32,
                )
            } == SOCKET_ERROR
            {
                return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }).into());
            }
        }
        socket.bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, port as u16)).into())?;
        socket.listen(4)?;
        socket.set_nonblocking(true)?;
        let listener = TcpListener::from_std(socket.into())?;
        let port = listener.local_addr()?.port();
        let device = Self(Arc::new(Owner {
            gate: Gate::default(),
            stop: CancellationToken::new(),
            port,
            diagnostic,
            state: Mutex::new(State {
                sessions: Vec::new(),
                completions: Vec::new(),
                current: None,
                generation: initial_generation,
                closed: false,
                accepting: None,
                heartbeat: None,
            }),
        }));
        let accepting = device.clone();
        let heartbeat = device.clone();
        let accept =
            MicroClose::start(async move { accepting.accept(listener).await.map_err(Arc::from) })?;
        let pulse = MicroClose::start(async move {
            heartbeat.heartbeat().await;
            Ok(())
        })?;
        device.0.gate.run(|| {
            let mut s = device.state();
            s.accepting = Some(accept);
            s.heartbeat = Some(pulse);
        });
        Ok(device)
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn port(&self) -> u16 {
        self.0.port
    }
    pub fn last_generation(&self) -> i64 {
        self.0.gate.run(|| self.state().generation)
    }
    pub fn healthy(&self) -> bool {
        self.0.gate.run(|| {
            let s = self.state();
            !s.closed
                && [&s.accepting, &s.heartbeat].into_iter().all(|work| {
                    work.as_ref()
                        .is_none_or(|work| !matches!(work.result(), Some(Err(_))))
                })
        })
    }
    pub fn ready_generation(&self) -> Option<i64> {
        self.0.gate.run(|| {
            let selected = {
                let s = self.state();
                if s.closed { None } else { s.current.clone() }
            };
            selected.filter(|s| s.ready()).map(|s| s.generation())
        })
    }
    pub fn send_microphone(
        &self,
        pressed: bool,
        expected: i64,
        original: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputReceipt {
        self.0.gate.run(|| {
            let selected = {
                let s = self.state();
                if s.closed { None } else { s.current.clone() }
            };
            selected.filter(|s| s.generation() == expected).map_or_else(
                || MicroInputReceipt {
                    phase: "not_submitted".into(),
                    pressed,
                    generation: expected,
                },
                |s| s.send_microphone(pressed, expected, original),
            )
        })
    }
    pub fn observe_voice(&self, expected: i64) -> MicroVoiceObservation {
        self.0.gate.run(|| {
            let selected = {
                let s = self.state();
                if s.closed { None } else { s.current.clone() }
            };
            selected.filter(|s| s.generation() == expected).map_or_else(
                || MicroVoiceObservation {
                    state: "unavailable".into(),
                    generation: expected,
                    dictation_sequence: 0,
                },
                |s| s.observe_voice(expected),
            )
        })
    }
    fn activate(&self, session: &MicroUsbSession) -> bool {
        let previous = self.0.gate.run(|| {
            let mut s = self.state();
            if s.closed || !s.sessions.iter().any(|s| s.same_connection(session)) {
                return Err(());
            }
            Ok(s.current.replace(session.clone()))
        });
        let Ok(previous) = previous else {
            return false;
        };
        if let Some(previous) = previous.filter(|s| !s.same_connection(session)) {
            previous.close();
        }
        true
    }
    async fn accept(&self, listener: TcpListener) -> MicroInputResult<()> {
        while !self.0.stop.is_cancelled() {
            let stream = tokio::select! {
                biased;
                _ = self.0.stop.cancelled() => break,
                value = listener.accept() => match value {
                    Ok((stream, _)) => stream,
                    Err(_) if self.0.stop.is_cancelled() => break,
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_nodelay(true)?;
            self.0.gate.run(|| -> MicroInputResult<()> {
                let mut s = self.state();
                if s.closed || s.sessions.len() == 4 {
                    return Ok(());
                }
                if s.generation == 2_097_151 {
                    return Err(
                        io::Error::other("Micro connection generation limit reached.").into(),
                    );
                }
                s.generation += 1;
                let owner: Weak<Owner> = Arc::downgrade(&self.0);
                let session = MicroUsbSession::tcp(
                    stream,
                    s.generation,
                    move |session| {
                        owner
                            .upgrade()
                            .is_some_and(|owner| MicroUsbDevice(owner).activate(session))
                    },
                    self.0.diagnostic.clone(),
                )?;
                s.sessions.push(session.clone());
                let run = session.start()?;
                let owned = session.clone();
                let device = self.clone();
                let completion = MicroClose::start(async move {
                    let _ = run.wait().await;
                    let result = owned.disposal().wait().await;
                    device.0.gate.run(|| {
                        let mut s = device.state();
                        // An unexpected original run/disposal fault does not
                        // certify the session's cleanup or remove its owner.
                        if result.is_ok() {
                            s.sessions.retain(|s| !s.same_connection(&owned));
                            if s.current
                                .as_ref()
                                .is_some_and(|s| s.same_connection(&owned))
                            {
                                s.current = None;
                            }
                        }
                        s.completions.retain(|(s, _)| !s.same_connection(&owned));
                    });
                    result
                })?;
                s.completions.push((session, completion));
                Ok(())
            })?;
        }
        Ok(())
    }
    async fn heartbeat(&self) {
        let period = Duration::from_secs(30);
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = self.0.stop.cancelled() => break,
                _ = timer.tick() => {
                    let snapshot = self.0.gate.run(|| self.state().sessions.clone());
                    for session in snapshot { session.check_heartbeat(); }
                }
            }
        }
    }
    pub fn disposal(&self) -> MicroInputResult<MicroClose> {
        self.0.gate.run(|| {
            let (snapshot, accepting, heartbeat) = {
                let mut s = self.state();
                // Preserve the original device's immediate repeat-dispose. The
                // attachment, rather than this listener, owns cached disposal.
                if s.closed {
                    return Ok(MicroClose::completed(Ok(())));
                }
                s.closed = true;
                self.0.stop.cancel();
                s.current = None;
                (
                    s.sessions.clone(),
                    s.accepting.clone().unwrap(),
                    s.heartbeat.clone().unwrap(),
                )
            };
            for session in snapshot {
                session.close();
            }
            let device = self.clone();
            MicroClose::start(async move {
                let (accept, pulse) = tokio::join!(accepting.wait(), heartbeat.wait());
                accept?;
                pulse?;
                let pending = device.0.gate.run(|| {
                    device
                        .state()
                        .completions
                        .iter()
                        .map(|(_, work)| work.clone())
                        .collect::<Vec<_>>()
                });
                let mut error = None;
                for work in pending {
                    if let Err(e) = work.wait().await {
                        error.get_or_insert(e);
                    }
                }
                error.map_or(Ok(()), Err)
            })
        })
    }
}
