use ivy_contracts::{IvyError, IvyResult, json::compare_versions};
use std::cmp::Ordering;

pub struct Requirements<'a> {
    pub node: &'a str,
    pub os: Option<&'a [&'a str]>,
    pub arch: Option<&'a [&'a str]>,
}
pub struct Runtime<'a> {
    pub node: &'a str,
    pub os: &'a str,
    pub arch: &'a str,
}

fn exact_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|c| c.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

pub fn runtime_requirements_satisfied(
    requirements: &Requirements<'_>,
    runtime: &Runtime<'_>,
) -> IvyResult<bool> {
    let (minimum, maximum) = requirements
        .node
        .strip_prefix(">=")
        .and_then(|text| text.split_once(" <"))
        .filter(|(min, max)| {
            [*min, *max].iter().all(|version| {
                let parts: Vec<_> = version.split('.').collect();
                parts.len() == 3
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
            })
        })
        .ok_or_else(|| {
            IvyError::new("invalid_arguments", "Package Node requirement is invalid.")
        })?;
    let current = runtime.node.strip_prefix('v').unwrap_or(runtime.node);
    if !exact_version(current) {
        return Err(IvyError::new(
            "unsupported_runtime",
            "Node runtime version is not an exact numeric version.",
        ));
    }
    Ok(compare_versions(current, minimum)? != Ordering::Less
        && compare_versions(current, maximum)? == Ordering::Less
        && requirements
            .os
            .is_none_or(|values| values.contains(&runtime.os))
        && requirements
            .arch
            .is_none_or(|values| values.contains(&runtime.arch)))
}
