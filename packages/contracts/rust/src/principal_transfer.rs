//! Host-configured ownership of retained evidence. Live authentication and new
//! requests continue to use the current principal.

use crate::{IvyError, IvyJson, IvyResult, JsString};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PrincipalTransfers {
    previous: HashMap<JsString, JsString>,
}

impl PrincipalTransfers {
    pub fn from_settings(settings: &IvyJson) -> IvyResult<Self> {
        let Some(transfers) = settings.get("principalTransfers") else {
            return Ok(Self::default());
        };
        let invalid = || {
            IvyError::new(
                "invalid_arguments",
                "Principal transfers require distinct current and previous identities.",
            )
        };
        let transfers = transfers.as_array().filter(|rows| rows.len() <= 16).ok_or_else(invalid)?;
        let mut previous = HashMap::with_capacity(transfers.len());
        let mut retired = HashSet::with_capacity(transfers.len());
        for row in transfers {
            let principal = |field| {
                row.get(field)
                    .and_then(IvyJson::as_js_string)
                    .filter(|value| (1..=200).contains(&value.0.len()))
                    .cloned()
                    .ok_or_else(invalid)
            };
            let current = principal("principalId")?;
            let prior = principal("previousPrincipalId")?;
            if row.as_object().is_none_or(|fields| fields.len() != 2)
                || !retired.insert(prior.clone())
                || previous.insert(current, prior).is_some()
            {
                return Err(invalid());
            }
        }
        if previous.keys().any(|current| retired.contains(current)) {
            return Err(invalid());
        }
        Ok(Self { previous })
    }

    pub fn previous(&self, principal: &JsString) -> Option<&JsString> {
        self.previous.get(principal)
    }

    pub fn owns(&self, current: &JsString, recorded: &JsString) -> bool {
        current == recorded || self.previous(current) == Some(recorded)
    }
}
