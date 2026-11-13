pub fn native_session_with_long_current_chain(
    device: &wacore::store::Device,
) -> wacore::libsignal::protocol::SessionRecord {
    use wacore::libsignal::protocol::{
        KeyPair, SessionChainComponents, SessionChainKeyComponents, SessionComponents,
        SessionRecord, SessionRecordComponents,
    };
    let state = |counter| {
        let sender = KeyPair::generate(&mut rand::rng());
        let receiver = KeyPair::generate(&mut rand::rng());
        SessionComponents {
            session_version: Some(3),
            local_identity_public: Some(device.identity_key.public_key.serialize().to_vec()),
            remote_identity_public: Some(device.identity_key.public_key.serialize().to_vec()),
            root_key: Some(vec![3; 32]),
            previous_counter: Some(0),
            sender_chain: Some(SessionChainComponents {
                sender_ratchet_key: Some(sender.public_key.serialize().to_vec()),
                sender_ratchet_key_private: Some(sender.private_key.serialize().to_vec()),
                chain_key: Some(SessionChainKeyComponents {
                    index: Some(counter),
                    key: Some(vec![6; 32]),
                }),
                message_keys: vec![],
            }),
            receiver_chains: vec![SessionChainComponents {
                sender_ratchet_key: Some(receiver.public_key.serialize().to_vec()),
                sender_ratchet_key_private: None,
                chain_key: Some(SessionChainKeyComponents {
                    index: Some(0),
                    key: Some(vec![8; 32]),
                }),
                message_keys: vec![],
            }],
            remote_registration_id: Some(device.registration_id),
            local_registration_id: Some(device.registration_id),
            alice_base_key: Some(
                KeyPair::generate(&mut rand::rng())
                    .public_key
                    .serialize()
                    .to_vec(),
            ),
            ..Default::default()
        }
    };
    SessionRecord::from_components(SessionRecordComponents {
        current_session: Some(state(2501)),
        previous_sessions: vec![state(0)],
    })
    .unwrap()
}
