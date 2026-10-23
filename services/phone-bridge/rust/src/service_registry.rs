//! Original public Phone tool metadata and reachable local service contracts.
pub use crate::contracts::phone_service_schema;
use ivy_contracts::{IvyJson, IvyResult};
pub struct PhoneTool {
    pub name: &'static str,
    pub input: &'static str,
    pub output: &'static str,
    pub read_only: bool,
    pub description: &'static str,
}
pub const PHONE_TOOLS: &[PhoneTool] = &[
    PhoneTool {
        name: "speakerVoices",
        input: "PhoneSpeakerVoiceQuery",
        output: "PhoneSpeakerVoices",
        read_only: true,
        description: "List audio speaker voices supported by the configured native Voice runtime for V3 (60-second cache). Optional callId includes selected/effective voices and restartRequired. This is separate from model/reasoning selection.",
    },
    PhoneTool {
        name: "selectSpeakerVoice",
        input: "PhoneSelectSpeakerVoice",
        output: "PhoneOperationResult",
        read_only: false,
        description: "Select an audio speaker voice for the original active Voice call, validated against phone_bridge_speaker_voices. Applies on its next Voice session; use phone_bridge_restart_voice explicitly to apply during this call. Does not restart, change the current audio, or change future calls' default. Requires callId, voiceId and operationId; replay returns the original outcome.",
    },
    PhoneTool {
        name: "voiceInputs",
        input: "PhoneCallQuery",
        output: "PhoneVoiceInputs",
        read_only: true,
        description: "Read up to four pending native approvals or questions for the original Voice task, including their exact reply schemas. Nothing is approved automatically.",
    },
    PhoneTool {
        name: "answerVoiceInput",
        input: "PhoneVoiceInputAnswer",
        output: "PhoneVoiceInputSubmitted",
        read_only: false,
        description: "Submit one reply to a current native Voice task input using its exact id and reply schema. A consumed or ended input cannot be sent again; submitted does not confirm the requested task action completed.",
    },
    PhoneTool {
        name: "probeLoopback",
        input: "PhoneStatusQuery",
        output: "PhoneLoopbackProbe",
        read_only: true,
        description: "Measure five seconds of Windows loopback activity while idle (administrator only). Play a test tone in another app; returns peak/sample counts without storing audio. New calls interrupt the probe.",
    },
    PhoneTool {
        name: "audioSetup",
        input: "PhoneStatusQuery",
        output: "PhoneAudioSetup",
        read_only: true,
        description: "Check audio device availability and mute settings for Windows routes (administrator only). Readiness covers device checks, not a live audio test; does not change devices.",
    },
    PhoneTool {
        name: "codecTest",
        input: "PhoneStatusQuery",
        output: "PhoneCodecTestResult",
        read_only: true,
        description: "Test installed codecs and resamplers while idle (administrator only). Uses synthetic audio without opening devices or calls; results are cached until the native process restarts.",
    },
    PhoneTool {
        name: "inventory",
        input: "PhoneStatusQuery",
        output: "PhoneInventory",
        read_only: true,
        description: "List Windows audio devices and supported call codecs for configuration (administrator only). Does not open audio streams or change defaults.",
    },
    PhoneTool {
        name: "reconnect",
        input: "PhoneReconnect",
        output: "SipRegistrationObservation",
        read_only: false,
        description: "Reconnect the configured SIP registration when no calls are active (administrator only). Keep operationId after uncertain replies.",
    },
    PhoneTool {
        name: "calls",
        input: "PhoneStatusQuery",
        output: "PhoneCalls",
        read_only: true,
        description: "List current calls visible to this caller; use phone.call for details.",
    },
    PhoneTool {
        name: "history",
        input: "PhoneHistoryQuery",
        output: "PhoneHistory",
        read_only: true,
        description: "Find current and completed calls by history. Returns this caller’s calls; administrators may inspect all calls.",
    },
    PhoneTool {
        name: "logs",
        input: "PhoneLogsQuery",
        output: "PhoneLogs",
        read_only: true,
        description: "Diagnose a call using retained audio, codec and transport logs. Logs can expire; original command receipts remain available.",
    },
    PhoneTool {
        name: "screen",
        input: "PhoneScreen",
        output: "PhoneCall",
        read_only: false,
        description: "Call a configured recipient with an automated announcement: 1 accepts, 2 declines. Include an audible automated-call disclosure. Read phone.call for the decision; bridgeScreening connects an accepted call to Windows audio.",
    },
    PhoneTool {
        name: "bridgeScreening",
        input: "PhoneCallQuery",
        output: "PhoneCall",
        read_only: false,
        description: "Connect an accepted screening call to Windows audio in the original call, without redialing or starting Voice.",
    },
    PhoneTool {
        name: "status",
        input: "PhoneStatusQuery",
        output: "PhoneServiceStatus",
        read_only: true,
        description: "Check phone readiness, configured recipient IDs and this caller’s active or incoming call. voiceModels lists models and their supportedReasoningEfforts from the configured Codex runtime, refreshed automatically with a 60-second cache. voiceModelsError reports catalog failures.",
    },
    PhoneTool {
        name: "request",
        input: "PhoneRequest",
        output: "PhoneCall",
        read_only: false,
        description: "Call a configured recipient. Choose route=voice and voicePrompt for a voice model conversation; the default route uses Windows audio. Authorized direct destinations require Windows. Success means admitted; check phone.call for connection.",
    },
    PhoneTool {
        name: "voiceCall",
        input: "PhoneVoiceCall",
        output: "PhoneCall",
        read_only: false,
        description: "Call a configured recipient through the voice agent. initialPrompt is required. If that recipient already has a Voice call, send the prompt to its current task and return that call instead of dialing again. Reusing operationId never sends twice. Otherwise deliver the prompt after connection; GPT-6.1 Sol/high is the default task selection. Use phone_bridge_status to check connection and the retained operation outcome.",
    },
    PhoneTool {
        name: "selectVoice",
        input: "PhoneSelectVoice",
        output: "PhoneOperationResult",
        read_only: false,
        description: "Change model and reasoningEffort for subsequent turns in the active Voice task. Read available model IDs and each model's supportedReasoningEfforts from phone_bridge_status.voiceModels; combinations are validated against the configured Codex runtime. Requires callId and a new operationId. Keeps the phone call and task running. Reusing operationId returns the original outcome.",
    },
    PhoneTool {
        name: "restartVoice",
        input: "PhoneRestartVoice",
        output: "PhoneOperationResult",
        read_only: false,
        description: "Create a new Voice task with the current model and reasoning, transfer the live Voice session and send the original greeting. Keeps the phone call connected. Requires callId and a new operationId; reuse it after lost replies to read the original outcome without another restart.",
    },
    PhoneTool {
        name: "accept",
        input: "PhoneAccept",
        output: "PhoneCall",
        read_only: false,
        description: "Accept the incoming call returned by phone.status for this caller.",
    },
    PhoneTool {
        name: "call",
        input: "PhoneCallQuery",
        output: "PhoneCallView",
        read_only: true,
        description: "Read an original call’s admission, connection, voice prompt and current state.",
    },
    PhoneTool {
        name: "operation",
        input: "PhoneOperationQuery",
        output: "PhoneOperationResult",
        read_only: true,
        description: "Read an original command outcome without repeating effects; null means that command has no retained intent.",
    },
    PhoneTool {
        name: "hangup",
        input: "PhoneCallQuery",
        output: "PhoneCallView",
        read_only: false,
        description: "End the original call or screening and release its audio resources. Uncertain cleanup remains blocked for recovery.",
    },
];
pub fn validate_phone_service(name: &str, value: &IvyJson) -> IvyResult<()> {
    crate::contracts::validate_service(name, value)
}
fn mcp(name: &str) -> Option<(String, &'static str)> {
    let (mapped, surface) = match name {
        "request" => return None,
        "status" | "call" | "operation" => ("phone_bridge_status", "ivy"),
        "voiceCall" => ("phone_bridge_call", "ivy"),
        "hangup" => ("phone_bridge_hangup", "ivy"),
        "selectVoice" => ("phone_bridge_select_voice", "ivy"),
        "speakerVoices" => ("phone_bridge_speaker_voices", "ivy"),
        "selectSpeakerVoice" => ("phone_bridge_select_speaker_voice", "ivy"),
        "restartVoice" => ("phone_bridge_restart_voice", "ivy"),
        "voiceInputs" => ("phone_bridge_voice_inputs", "ivy"),
        "answerVoiceInput" => ("phone_bridge_answer_voice_input", "ivy"),
        "probeLoopback" | "audioSetup" | "codecTest" | "inventory" | "logs" => {
            ("phone_bridge_diagnostics", "ivy_dev")
        }
        "reconnect" => ("phone_bridge_reconnect", "ivy_dev"),
        _ => {
            let mut mapped = String::from("phone_bridge_");
            for letter in name.chars() {
                if letter.is_ascii_uppercase() {
                    mapped.push('_');
                    mapped.push(letter.to_ascii_lowercase());
                } else {
                    mapped.push(letter);
                }
            }
            return Some((mapped, "ivy_dev"));
        }
    };
    Some((mapped.into(), surface))
}
pub fn phone_registry() -> IvyResult<IvyJson> {
    let tools = PHONE_TOOLS
        .iter()
        .map(|tool| {
            let mut value = IvyJson::object([
                ("namespace", "phone".into()),
                ("name", tool.name.into()),
                ("interfaceVersion", "1.0.0".into()),
                ("description", tool.description.into()),
                ("inputSchema", phone_service_schema(tool.input)?),
                ("outputSchema", phone_service_schema(tool.output)?),
                (
                    "annotations",
                    IvyJson::object([
                        ("readOnlyHint", tool.read_only.into()),
                        ("idempotentHint", true.into()),
                    ]),
                ),
            ]);
            if let Some((name, surface)) = mcp(tool.name) {
                value.insert(
                    "discovery",
                    IvyJson::object([(
                        "mcp",
                        IvyJson::object([("name", name.into()), ("surface", surface.into())]),
                    )]),
                );
            }
            Ok(value)
        })
        .collect::<IvyResult<Vec<_>>>()?;
    Ok(IvyJson::object([
        ("discoveryHint", "PhoneBridge lets agents call configured recipients through a voice agent. Use phone_bridge_status, phone_bridge_call with initialPrompt, phone_bridge_select_voice, phone_bridge_restart_voice and phone_bridge_hangup. Native approvals and questions are available through phone_bridge_voice_inputs and phone_bridge_answer_voice_input.".into()),
        ("namespaces", IvyJson::Array(vec![IvyJson::object([
            ("namespace", "phone".into()), ("description", "Phone calls with a voice model, Windows audio or automated announcements".into()),
            ("guideMarkdown", "Use phone_bridge_status for readiness, configured recipient IDs, original call state, current Voice model selection and original operation outcomes. phone_bridge_call requires recipientId, operationId and a nonempty initialPrompt. PhoneBridge creates a local task with GPT-6.1 Sol/high by default and sends the prompt after connection. During the call, *1<M><R># selects model 1 Luna, 2 Sol or 3 Astra and reasoning 1 low, 2 medium, 3 high, 4 xhigh, 5 max or 6 ultra (not Luna). Selection leaves the current task running; *0# creates a new task with the selected model and reasoning, then starts Voice there. Use phone_bridge_hangup to end the call. phone_bridge_status.voiceModels discovers all picker-visible models and their supportedReasoningEfforts from the configured Codex home via model/list, cached for up to 60 seconds. Check voiceModelsError if discovery is unavailable. phone_bridge_select_voice accepts these exact model IDs and efforts without a fixed allowlist. It changes the current task; phone_bridge_restart_voice creates and transfers to a fresh task with that selection and the original greeting. Both require callId and operationId, keep the phone connection, and return the retained operation. Use a new operationId only for a new action; read a lost reply via phone_bridge_status with callId and operationId. Audio speaker voices are separate: phone_bridge_speaker_voices lists native V3 voices and, with callId, selected/effective voice and restartRequired. phone_bridge_select_speaker_voice takes callId, voiceId and operationId; it selects for that call's next Voice session without restarting. Use phone_bridge_restart_voice explicitly to apply now; future calls retain their configured default. A replay returns the original selection outcome. If the recipient already has an incoming or outgoing Voice call, phone_bridge_call forwards initialPrompt unchanged to its current task and returns the existing callId. It waits for setup or a task switch and retains the send under the request operationId, so retries cannot duplicate it. The internal phone.request method also supports the Windows audio route and authorized direct destinations. phone.screen plays an announcement and collects 1/2; phone.bridgeScreening connects an accepted call to Windows audio. Keep operationId and callId after lost replies. Admission does not prove connection: inspect the original call or operation through phone_bridge_status. Development operators can use ivy_dev diagnostics and reconnect tools for recovery.".into()), ("tools", IvyJson::Array(tools)),
            ("topics", IvyJson::Array(vec![])), ("inventoryKinds", IvyJson::Array(vec![])),
        ])])), ("contracts", IvyJson::Array(vec![])), ("requiredContracts", IvyJson::Array(vec![])),
    ]))
}
#[cfg(test)]
#[path = "../../../../tests/rust/phone_service_registry.rs"]
mod tests;
