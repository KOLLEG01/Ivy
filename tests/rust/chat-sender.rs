#[path = "chat-config-cases.rs"]
mod config_cases;
#[path = "../../services/chat-bridge/rust/src/whatsapp/sender.rs"]
mod sender;
use sender::{MessageKey, admitted_sender, jid_phone, needs_lid_mapping, phone};

#[test]
fn configured_phone_and_device_jids_require_exact_ascii_identities() {
    for value in ["15555550200", "+15555550200", "0015555550200"] {
        assert_eq!(phone(value), Some("15555550200"));
    }
    for value in [
        "15555550200@s.whatsapp.net",
        "15555550200:7@s.whatsapp.net",
        "15555550200@c.us",
    ] {
        assert_eq!(jid_phone(value), Some("15555550200"));
    }
    for value in [
        "",
        "01234567",
        "123456",
        "1234567890123456",
        " 15555550200",
        "15555550200\n",
        "١٢٣٤٥٦٧",
        "１２３４５６７",
    ] {
        assert!(phone(value).is_none(), "{value:?}");
    }
    for value in [
        "123@lid",
        "123@g.us",
        "15555550200:@c.us",
        "15555550200:１@c.us",
        "15555550200:1:2@c.us",
        "15555550200@S.WHATSAPP.NET",
    ] {
        assert!(jid_phone(value).is_none(), "{value:?}");
    }
}

#[test]
fn sender_admission_refuses_groups_own_messages_and_conflicting_lid_mappings() {
    let phones = ["+15555550200", "0015555550201"];
    let mut key = MessageKey {
        id: "message",
        remote_jid: "15555550200:7@s.whatsapp.net",
        remote_jid_alt: "",
        from_me: false,
        participant: "",
    };
    assert_eq!(admitted_sender(&key, None, &phones), Some(0));
    assert!(!needs_lid_mapping(&key));
    for jid in ["15555550999@s.whatsapp.net", "123@g.us", "15555550200@c.us"] {
        key.remote_jid = jid;
        assert!(admitted_sender(&key, None, &phones).is_none());
        assert!(!needs_lid_mapping(&key));
    }
    key.remote_jid = "123@lid";
    assert!(needs_lid_mapping(&key));
    assert!(admitted_sender(&key, None, &phones).is_none());
    assert_eq!(
        admitted_sender(&key, Some("15555550200@s.whatsapp.net"), &phones),
        Some(0)
    );
    key.remote_jid_alt = "15555550201@s.whatsapp.net";
    assert_eq!(admitted_sender(&key, None, &phones), Some(1));
    assert!(admitted_sender(&key, Some("15555550200@s.whatsapp.net"), &phones).is_none());
    for guard in ["own", "participant", "empty-id"] {
        key.from_me = guard == "own";
        key.participant = if guard == "participant" { "sender" } else { "" };
        key.id = if guard == "empty-id" { "" } else { "message" };
        assert!(admitted_sender(&key, None, &phones).is_none());
        assert!(!needs_lid_mapping(&key));
    }
}
