//! Pure transport admission, before storage, scheduling or content logging.
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat, hash::hash_json,
    json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use std::collections::HashSet;

pub fn same_channel(a: &Json, b: &Json) -> bool {
    ["adapter", "accountId", "channelId"]
        .iter()
        .all(|key| a.get(key) == b.get(key))
}

// Match ordinary request.expectedBridge access before contract validation:
// null throws, while an absent property reaches the JSON-domain check.
pub(crate) fn expected_bridge(request: &Json) -> IvyResult<&Json> {
    if request.is_null() {
        return Err(IvyError::internal());
    }
    request
        .get("expectedBridge")
        .ok_or_else(|| IvyError::new("invalid_arguments", "Value is outside the JSON domain."))
}

pub struct ChatAdmission {
    definition: Json,
    definition_hash: String,
}
pub struct AdmittedSend {
    pub identity: Json,
    pub request_hash: String,
}
impl ChatAdmission {
    pub fn new(definition: &Json) -> IvyResult<Self> {
        validate_chat("Definition", definition)?;
        if canonical(definition, MANAGEMENT_FRAME_BYTES)?.len() > 128 * 1024 {
            return Err(IvyError::new(
                "limit_exceeded",
                "ChatBridge definition exceeds its canonical byte limit.",
            ));
        }
        let project = definition.required("project")?;
        if project.get("namespace") != Some(&Json::from("codex"))
            || project.get("kind") != Some(&Json::from("project"))
        {
            return Err(IvyError::new(
                "chat_definition_mismatch",
                "ChatBridge needs an explicitly configured native project owner.",
            ));
        }
        let mut identities = HashSet::new();
        for configured in definition.required("channels")?.as_array().unwrap() {
            if !identities.insert(hash_json(configured.required("channel")?)?) {
                return Err(IvyError::new(
                    "chat_definition_mismatch",
                    "ChatBridge channel identities must be unique.",
                ));
            }
        }
        Ok(Self {
            definition: definition.clone(),
            definition_hash: hash_json(definition)?,
        })
    }
    pub fn definition(&self) -> &Json {
        &self.definition
    }
    pub fn definition_hash(&self) -> &str {
        &self.definition_hash
    }
    pub fn channels_for(&self, _caller: &JsString) -> Json {
        self.definition.get("channels").unwrap().clone()
    }
    // Reader/notice/either have identical admission in the current contract.
    pub fn expected(&self, caller: &JsString) -> Json {
        Json::object([
            (
                "principalId",
                self.definition.get("principalId").unwrap().clone(),
            ),
            ("callerPrincipalId", Json::String(caller.clone())),
            (
                "workspaceId",
                self.definition.get("workspaceId").unwrap().clone(),
            ),
            ("definitionHash", self.definition_hash.as_str().into()),
        ])
    }
    pub fn authorize(
        &self,
        caller: &JsString,
        expected: &Json,
        channel: Option<&Json>,
    ) -> IvyResult<()> {
        let actual = self.expected(caller);
        validate_chat("ExpectedBridge", expected)?;
        // Retained requests may contain an earlier definition hash. Only these
        // three fields establish the current Main/caller scope.
        if ["principalId", "callerPrincipalId", "workspaceId"]
            .iter()
            .any(|key| actual.get(key) != expected.get(key))
        {
            return Err(IvyError::new(
                "chat_definition_mismatch",
                "The request belongs to another Main or caller.",
            ));
        }
        if let Some(channel) = channel {
            validate_chat("Channel", channel)?;
            if !self
                .definition
                .get("channels")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|configured| same_channel(configured.get("channel").unwrap(), channel))
            {
                return Err(IvyError::new(
                    "chat_sender_refused",
                    "The exact account and channel are not configured.",
                ));
            }
        }
        Ok(())
    }
    pub fn send(&self, caller: &JsString, request: &Json) -> IvyResult<AdmittedSend> {
        self.authorize(
            caller,
            expected_bridge(request)?,
            request
                .get("channel")
                .filter(|value| crate::main_context::truthy(value)),
        )?;
        validate_chat("SendRequest", request)?;
        let payload = request.required("payload")?;
        let images = payload.required("images")?.as_array().unwrap();
        if payload
            .required("text")?
            .as_js_string()
            .unwrap()
            .0
            .is_empty()
            && images.is_empty()
        {
            return Err(IvyError::new(
                "invalid_arguments",
                "A chat input needs text or an image.",
            ));
        }
        if images
            .iter()
            .map(|image| image.get("byteLength").unwrap().as_f64().unwrap())
            .sum::<f64>()
            > (4 * 1024 * 1024) as f64
        {
            return Err(IvyError::new(
                "limit_exceeded",
                "Chat input image bytes exceed the combined limit.",
            ));
        }
        let identity = Json::object([
            ("channel", request.required("channel")?.clone()),
            ("senderPrincipalId", Json::String(caller.clone())),
            ("messageId", request.required("messageId")?.clone()),
        ]);
        let request_hash = hash_json(&Json::object([
            (
                "workspaceId",
                self.definition.required("workspaceId")?.clone(),
            ),
            ("definitionHash", self.definition_hash.as_str().into()),
            ("identity", identity.clone()),
            ("binding", request.required("expectedBinding")?.clone()),
            ("payload", payload.clone()),
        ]))?;
        Ok(AdmittedSend {
            identity,
            request_hash,
        })
    }
}
