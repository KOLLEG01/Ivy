//! Language-aware display text using compiled Rust data. Identifiers, hashes and
//! persistence keys use ordinal comparison instead of this display policy.
use crate::{IvyError, IvyResult, JsString};
use icu_casemap::CaseMapper;
use icu_collator::{Collator, CollatorBorrowed, options::CollatorOptions};
use icu_locale_core::{LanguageIdentifier, Locale};
use std::{borrow::Cow, cmp::Ordering};

pub struct LocaleComparator {
    collator: CollatorBorrowed<'static>,
    language: LanguageIdentifier,
}
impl LocaleComparator {
    /// Capture the account's preferred language; unspecified locales use root
    /// Unicode data. No binary, filesystem location or mutable process owner is needed.
    pub fn system() -> IvyResult<Self> {
        let locale = sys_locale::get_locale().unwrap_or_else(|| "und".into());
        Self::for_locale(&locale).or_else(|_| Self::for_locale("und"))
    }
    pub fn for_locale(locale: &str) -> IvyResult<Self> {
        let locale: Locale = locale
            .parse()
            .map_err(|_| IvyError::new("invalid_arguments", "A valid language tag is required."))?;
        let language = locale.id.clone();
        let collator =
            Collator::try_new(locale.into(), CollatorOptions::default()).map_err(|_| {
                IvyError::new(
                    "locale_unavailable",
                    "Display language data is unavailable.",
                )
            })?;
        Ok(Self { collator, language })
    }
    pub fn compare_text(&self, left: &str, right: &str) -> Ordering {
        self.collator.compare(left, right)
    }
    pub fn lowercase_text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        CaseMapper::new().lowercase_to_string(text, &self.language)
    }
    /// Display comparison may treat invalid units as Unicode replacement
    /// characters; the source JSON and persistence keys remain lossless.
    pub fn compare(&self, left: &JsString, right: &JsString) -> Ordering {
        self.collator.compare_utf16(&left.0, &right.0)
    }
    pub fn lowercase(&self, value: &JsString) -> IvyResult<JsString> {
        Ok(ivy_contracts::json::map_unicode(&value.0, |text| {
            self.lowercase_text(text).into_owned()
        }))
    }
}
