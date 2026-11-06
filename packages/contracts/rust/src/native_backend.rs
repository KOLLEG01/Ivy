//! Stable backend/account selection, independent of a model's display name.
use crate::{IvyError, IvyJson, IvyResult, agent_schema::validate_agent};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum NativeProvider {
    Codex,
    Claude,
}
impl NativeProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct BackendIdentity {
    id: String,
    provider: NativeProvider,
    account_id: String,
}
impl BackendIdentity {
    pub fn new(id: String, provider: NativeProvider, account_id: String) -> IvyResult<Self> {
        let value = Self {
            id,
            provider,
            account_id,
        };
        validate_agent("BackendIdentity", &value.json())?;
        Ok(value)
    }
    pub fn from_json(value: &IvyJson) -> IvyResult<Self> {
        validate_agent("BackendIdentity", value)?;
        Ok(Self {
            id: value.required("backendId")?.as_str()?,
            provider: match value.required("provider")?.as_str()?.as_str() {
                "codex" => NativeProvider::Codex,
                "claude" => NativeProvider::Claude,
                _ => unreachable!("validated provider"),
            },
            account_id: value.required("accountId")?.as_str()?,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn provider(&self) -> NativeProvider {
        self.provider
    }
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("backendId", self.id.clone().into()),
            ("provider", self.provider.as_str().into()),
            ("accountId", self.account_id.clone().into()),
        ])
    }
    pub fn optional(value: Option<&IvyJson>) -> IvyResult<Option<Self>> {
        value.map(Self::from_json).transpose()
    }
    pub fn matches(value: &IvyJson, expected: Option<&Self>) -> bool {
        match (value.get("backend"), expected) {
            (None, None) => true,
            (Some(actual), Some(expected)) => {
                Self::from_json(actual).is_ok_and(|actual| &actual == expected)
            }
            _ => false,
        }
    }
    /// Management selectors never alter the native arguments nested in `params`.
    pub fn arguments(&self, mut arguments: IvyJson) -> IvyResult<IvyJson> {
        if arguments.as_object().is_none() {
            return Err(IvyError::new(
                "invalid_arguments",
                "Backend management arguments must be an object.",
            ));
        }
        if arguments.get("backend").is_some() && !Self::matches(&arguments, Some(self)) {
            return Err(IvyError::new(
                "native_backend_mismatch",
                "Management arguments select another backend or account.",
            ));
        }
        arguments.insert("backend", self.json());
        Ok(arguments)
    }
    pub fn select_arguments(expected: Option<&Self>, arguments: IvyJson) -> IvyResult<IvyJson> {
        if arguments.as_object().is_none() {
            return Err(IvyError::new(
                "invalid_arguments",
                "Backend management arguments must be an object.",
            ));
        }
        if let Some(expected) = expected {
            return expected.arguments(arguments);
        }
        if arguments.get("backend").is_some() {
            return Err(IvyError::new(
                "native_backend_mismatch",
                "Management arguments select another backend or account.",
            ));
        }
        Ok(arguments)
    }
}
