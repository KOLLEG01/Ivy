use super::*;
use ivy_phone_bridge::{
    media::{MediaError, RtcpRuntime, RtcpWallTime},
    native_sip::NativeSipCall,
};

#[derive(Default)]
pub(super) struct FailureEvents {
    pub call: Mutex<Option<NativeSipCall>>,
    pub receipts: Mutex<Vec<IvyJson>>,
}
impl SipReceiveEvents for FailureEvents {
    fn digit(&self, _: char, _: ivy_phone_bridge::dtmf::InputSource) {}
    fn rtcp(&self, _: std::net::SocketAddr, _: &[u8]) {}
    fn failed(&self) {
        let call = self.call.lock().unwrap().as_ref().unwrap().clone();
        let receipt = parse(
            &ivy_phone_bridge::native_sip_status::call_json(&call).unwrap(),
            65536,
        )
        .unwrap();
        assert!(
            call.observation().ending,
            "Original owner ends before external failure listeners"
        );
        self.receipts.lock().unwrap().push(receipt);
        panic!("Synthetic external failure listener");
    }
}
pub(super) fn fail_media(media: &SipMediaSession, sdp: &str) {
    let invalid = SdpDocument::parse(&sdp.replace("PCMA/8000", "PCMA/8000/2")).unwrap();
    // Static payload matching reaches the actual unsupported PCM constructor.
    media
        .set_remote_description(SdpType::Offer, &invalid)
        .unwrap();
    assert!(media.observation().failed);
}
#[derive(Default)]
struct StartClock(AtomicBool);
impl RtcpRuntime for StartClock {
    fn now(&self) -> Result<RtcpWallTime, MediaError> {
        Clock.now()
    }
    fn interval_ms(&self) -> Result<u16, MediaError> {
        if self.0.load(Ordering::SeqCst) {
            Err(MediaError::ClockUnavailable)
        } else {
            Ok(6000)
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_media_start_failure_and_early_remote_end_preserve_terminal_notification() {
    use rsipstack::sip::{Method, prelude::*};
    for delayed in [false, true] {
        for remote_end in [false, true] {
            let clock = Arc::new(StartClock::default());
            let events = Arc::new(FailureEvents::default());
            let (owner, factory, routes, config) =
                actual_owner_with(43000, 43998, clock.clone(), events.clone());
            let remaining = Arc::new(Remaining::default());
            remaining.close_gate.release();
            let handler = Arc::new(NativeSipCommands::new(
                owner.clone(),
                remaining.clone(),
                |_| Box::pin(async { Ok(()) }),
                closed_platform_views(),
            ));
            let dispatcher =
                Arc::new(NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap());
            assert!(
                dispatcher
                    .execute(command("configure", &config, 1, 1))
                    .await
                    .outcome()
                    .error_code()
                    .is_none()
            );
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let (rtp, _rtcp) = pair().await;
            let target = owner.observation().endpoint.unwrap();
            let sdp = format!(
                "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=sendrecv\r\n",
                rtp.local_addr().unwrap().port()
            );
            super::answer::send(
                &peer,
                target,
                "INVITE",
                1,
                "z9hG4bK-owned-answer",
                "",
                if delayed { "" } else { &sdp },
            )
            .await;
            assert_eq!(
                super::incoming::receive(&peer, 4000)
                    .await
                    .unwrap()
                    .status_code
                    .code(),
                180
            );
            let call = owner.find(&owner.observation().calls[0].id).unwrap();
            let id = call.observation().id;
            *events.call.lock().unwrap() = Some(call.clone());
            let selected = format!(r#"{{"callId":"{id}"}}"#);
            for (m, p, op) in [
                ("call.claim", selected.clone(), 2),
                (
                    "call.features",
                    format!(r#"{{"callId":"{id}","challenge":null}}"#),
                    3,
                ),
            ] {
                assert!(
                    dispatcher
                        .execute(command(m, &p, op, op))
                        .await
                        .outcome()
                        .error_code()
                        .is_none()
                );
            }
            let params = format!(r#"{{"callId":"{id}","waiting":false}}"#);
            let d = dispatcher.clone();
            let pending =
                tokio::spawn(async move { d.execute(command("call.answer", &params, 4, 4)).await });
            let response = super::incoming::receive(&peer, 4000).await.unwrap();
            assert_eq!(response.status_code.code(), 200);
            let tag = response
                .to_header()
                .unwrap()
                .tag()
                .unwrap()
                .unwrap()
                .value()
                .to_string();
            if remote_end {
                super::answer::send(&peer, target, "BYE", 2, "z9hG4bK-early-bye", &tag, "").await;
                assert_eq!(
                    super::incoming::receive(&peer, 4000)
                        .await
                        .unwrap()
                        .status_code
                        .code(),
                    200
                );
                assert!(call.observation().ending);
            } else {
                clock.0.store(true, Ordering::SeqCst);
            }
            super::answer::send(
                &peer,
                target,
                "ACK",
                1,
                "z9hG4bK-owned-ack",
                &tag,
                if delayed { &sdp } else { "" },
            )
            .await;
            if !remote_end {
                let (bye, remote) = crate::sip_host::received(&peer).await;
                assert_eq!(bye.method, Method::Bye);
                crate::sip_host::send(&peer, remote, &crate::sip_host::response(&bye, 200, "", ""))
                    .await;
            }
            let answer = pending.await.unwrap();
            let answered = value(answer.outcome());
            assert_eq!(
                answered.get("state").unwrap().as_str().as_deref(),
                Some("local_ended")
            );
            assert_eq!(
                answered.get("error").unwrap(),
                &if remote_end {
                    IvyJson::Null
                } else {
                    "media_failed".into()
                }
            );
            assert!(!call.media().unwrap().observation().started);
            assert_eq!(
                events.receipts.lock().unwrap().len(),
                usize::from(!remote_end)
            );
            assert_eq!(call.features().observation().access, "not_required");
            trace(
                &format!("native-terminal-start-{delayed}-{remote_end}"),
                IvyJson::object([
                    ("answered", answered),
                    (
                        "failureCallbacks",
                        IvyJson::Array(events.receipts.lock().unwrap().clone()),
                    ),
                    ("originalMediaStarted", false.into()),
                    ("injectedRtcpClockFailure", (!remote_end).into()),
                ]),
            );
            events.call.lock().unwrap().take();
            owner.release(&call).unwrap().wait().await.unwrap();
            handler.shutdown().await.unwrap();
            assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
            assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        }
    }
}
