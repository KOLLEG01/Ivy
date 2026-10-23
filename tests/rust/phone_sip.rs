use ivy_phone_bridge::sip::{CallAction::*, CallError, CallOutcome, DelayedOfferCall, FinalPhase};
use ivy_phone_bridge::sip::{Dialog, DialogRequest, ReinviteAdmission::*};

fn dialog_request(sequence: i32) -> DialogRequest<'static> {
    DialogRequest {
        call_id: "original",
        from_tag: "remote",
        to_tag: "local",
        sequence,
        branch: "original-branch",
    }
}

#[test]
fn dialog_fences_foreign_and_stale_bye_before_original_call_cleanup() {
    let mut dialog = Dialog::new("original".into(), "local".into(), "remote".into(), 5);
    for changed in 0..3 {
        let mut foreign = dialog_request(100);
        match changed {
            0 => foreign.call_id = "replacement",
            1 => foreign.from_tag = "foreign",
            _ => foreign.to_tag = "foreign",
        }
        assert_eq!(dialog.bye(&foreign), None);
        assert_eq!(dialog.remote_sequence(), 5);
    }
    for stale in [i32::MIN, 0, 4, 5] {
        assert_eq!(dialog.bye(&dialog_request(stale)), Some(500));
    }
    assert_eq!(dialog.bye(&dialog_request(6)), Some(200));
    assert_eq!(dialog.bye(&dialog_request(6)), Some(500));
    assert_eq!(dialog.remote_sequence(), 6);
}

#[test]
fn reinvite_overlap_cancel_ack_and_timer_belong_to_exact_original_transaction() {
    let mut dialog = Dialog::new("original".into(), "local".into(), "remote".into(), 0);
    let mut foreign_dialog = dialog_request(100);
    foreign_dialog.from_tag = "foreign";
    assert_eq!(dialog.begin_reinvite(&foreign_dialog, 9, true), Foreign);
    assert_eq!(dialog.cancel_reinvite(&foreign_dialog), None);
    assert_eq!(dialog.remote_sequence(), 0);
    assert_eq!(
        dialog.begin_reinvite(&dialog_request(1), 10, false),
        Refused(481)
    );
    assert_eq!(
        dialog.begin_reinvite(&dialog_request(1), 10, true),
        Accepted
    );
    assert_eq!(
        dialog.begin_reinvite(&dialog_request(1), 11, true),
        Refused(481)
    );
    assert_eq!(
        dialog.begin_reinvite(&dialog_request(2), 11, true),
        Refused(491)
    );
    assert_eq!(dialog.cancel_reinvite(&dialog_request(1)), Some(200));
    let mut foreign = dialog_request(1);
    foreign.branch = "replacement";
    assert_eq!(dialog.cancel_reinvite(&foreign), Some(481));
    assert!(!dialog.finish_reinvite(11));
    assert!(!dialog.acknowledge_reinvite(11, &dialog_request(1)));
    assert!(!dialog.acknowledge_reinvite(10, &dialog_request(2)));
    // Unlike CANCEL, ACK uses a fresh branch for a successful dialog response.
    assert!(dialog.acknowledge_reinvite(10, &foreign));
    assert_eq!(
        dialog.begin_reinvite(&dialog_request(2), 12, true),
        Accepted
    );
    assert!(!dialog.finish_reinvite(10));
    assert!(dialog.finish_reinvite(12));
    assert_eq!(dialog.cancel_reinvite(&dialog_request(2)), Some(481));
}

fn sent() -> DelayedOfferCall {
    let mut call = DelayedOfferCall::default();
    call.start().unwrap();
    assert!(call.sending(1).unwrap());
    call
}

#[test]
fn cancel_waits_for_provisional_and_late_success_is_acked_before_bye() {
    // LateOfferTests.DelayedOfferCancelled: cancellation retains the original
    // slot. A late 200 cannot connect; reject its media in ACK, then send BYE.
    for cancel_at in 0..4 {
        let mut call = sent();
        if cancel_at == 0 {
            assert!(call.hangup().is_empty());
        }
        assert_eq!(
            call.provisional(1, false),
            if cancel_at == 0 { vec![Cancel] } else { vec![] }
        );
        if cancel_at == 1 {
            assert_eq!(call.hangup(), vec![Cancel]);
        }
        assert_eq!(call.close(), Err(CallError::CleanupUnconfirmed));
        assert!(call.final_response(1, 200));
        if cancel_at == 2 {
            assert!(call.hangup().is_empty());
        }
        assert_eq!(
            call.negotiated(true, false).unwrap(),
            Acknowledge {
                reject_media: cancel_at < 3
            }
        );
        if cancel_at == 3 {
            assert!(call.hangup().is_empty());
        }
        assert_eq!(
            call.acknowledged(true, false).unwrap(),
            vec![Bye {
                authenticated: false
            }]
        );
        assert!(!call.is_active());
        assert_eq!(call.outcome(), Some(CallOutcome::Ended));
        assert!(call.hangup().is_empty());
        assert_eq!(call.close(), Err(CallError::CleanupUnconfirmed));
        assert!(call.bye_response(200, false).unwrap().is_empty());
        assert!(call.cleanup_confirmed());
        call.close().unwrap();
    }
}

#[test]
fn authenticated_invite_is_single_and_stale_events_cannot_finish_it() {
    let mut call = sent();
    assert!(call.provisional(1, false).is_empty());
    call.authenticated(1, 2).unwrap();
    assert!(call.provisional(1, true).is_empty());
    assert!(!call.final_response(1, 403));
    assert!(!call.final_response(1, 200));
    assert_eq!(call.authenticated(1, 3), Err(CallError::StaleTransaction));
    assert_eq!(call.authenticated(2, 3), Err(CallError::WrongPhase));
    assert!(call.hangup().is_empty()); // prior provisional is not current.
    assert_eq!(call.provisional(2, false), vec![Cancel]);
    assert!(!call.final_response(2, 487));
    assert_eq!(call.phase(), FinalPhase::Refused);
    assert_eq!(call.outcome(), Some(CallOutcome::Ended));
    call.close().unwrap();
}

#[test]
fn acknowledgement_and_media_failures_keep_uncertain_cleanup_owned() {
    for fail_ack in [true, false] {
        let mut call = sent();
        assert!(call.final_response(1, 200));
        assert_eq!(
            call.negotiated(true, false).unwrap(),
            Acknowledge {
                reject_media: false
            }
        );
        let actions = call.acknowledged(!fail_ack, false).unwrap();
        if fail_ack {
            assert_eq!(actions, vec![Uncertain]);
        } else {
            assert_eq!(actions, vec![StartMedia]);
            assert_eq!(call.media_started(false, false).unwrap(), vec![Uncertain]);
        }
        assert_eq!(call.outcome(), Some(CallOutcome::Unknown));
        assert!(!call.is_active());
        assert!(call.fail().is_empty());
        assert_eq!(call.close(), Err(CallError::CleanupUnconfirmed));
    }
}

#[test]
fn media_start_cancellation_and_remote_bye_never_publish_connected() {
    for remote_bye in [false, true] {
        let mut call = sent();
        assert!(call.final_response(1, 200));
        call.negotiated(true, false).unwrap();
        assert_eq!(call.acknowledged(true, false).unwrap(), vec![StartMedia]);
        if remote_bye {
            assert_eq!(call.remote_bye(), vec![Ended]);
        } else {
            assert_eq!(
                call.hangup(),
                vec![Bye {
                    authenticated: false
                }]
            );
        }
        assert!(call.ring_deadline().is_empty());
        assert!(call.media_started(true, false).unwrap().is_empty());
        assert_eq!(call.outcome(), Some(CallOutcome::Ended));
        assert!(!call.is_active());
        if !remote_bye {
            call.bye_response(481, false).unwrap();
        }
        call.close().unwrap();
    }
}

#[test]
fn connected_call_preserves_answer_and_bounds_bye_authentication() {
    let mut call = sent();
    assert!(call.final_response(1, 200));
    assert!(!call.final_response(1, 200)); // duplicate belongs to saved ACK.
    call.negotiated(true, false).unwrap();
    call.acknowledged(true, false).unwrap();
    assert!(call.media_started(true, false).unwrap().is_empty());
    assert_eq!(call.outcome(), Some(CallOutcome::Connected));
    assert!(call.is_active());
    assert_eq!(
        call.hangup(),
        vec![Bye {
            authenticated: false
        }]
    );
    assert!(!call.is_active());
    assert_eq!(
        call.bye_response(407, true).unwrap(),
        vec![Bye {
            authenticated: true
        }]
    );
    assert_eq!(call.bye_response(407, true).unwrap(), vec![Uncertain]);
    assert_eq!(call.outcome(), Some(CallOutcome::Connected));
    assert!(!call.cleanup_confirmed());
    call.bye_response(481, true).unwrap();
    call.close().unwrap();
}

#[test]
fn reliable_provisional_invalid_sdp_and_unstarted_stop_are_fail_closed() {
    let mut call = sent();
    assert_eq!(call.provisional(1, true), vec![Cancel, Ended]);
    assert!(call.final_response(1, 200));
    assert_eq!(
        call.negotiated(false, false).unwrap(),
        Acknowledge { reject_media: true }
    );
    assert_eq!(
        call.acknowledged(true, false).unwrap(),
        vec![Bye {
            authenticated: false
        }]
    );
    let mut unsent = DelayedOfferCall::default();
    unsent.start().unwrap();
    assert_eq!(unsent.ring_deadline(), vec![Ended]);
    assert!(!unsent.sending(1).unwrap());
    assert_eq!(unsent.start(), Err(CallError::AlreadyAttempted));
    unsent.close().unwrap();
    let mut no_destination = DelayedOfferCall::default();
    no_destination.start().unwrap();
    no_destination.no_destination().unwrap();
    no_destination.close().unwrap();
}
