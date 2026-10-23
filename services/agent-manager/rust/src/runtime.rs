//! Native defaults for the AgentManager executable.
use crate::{native_launch::NativeHomeInputs, service_kernel::AgentRuntimeBindings};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyResult, JsString};
use ivy_host_runtime::{config::InstanceConfig, node_path};
use ivy_sdk::rpc::{ClientOptions, HiveClient};
use std::{cmp::Ordering, collections::BTreeMap, path::PathBuf, sync::Arc};

pub fn home_inputs() -> IvyResult<NativeHomeInputs> {
    let paths = node_path::Context::current().map_err(|_| IvyError::internal())?;
    let environment: BTreeMap<_, _> = std::env::vars_os()
        .map(|(key, value)| {
            (
                node_path::from_native(&PathBuf::from(key)),
                node_path::from_native(&PathBuf::from(value)),
            )
        })
        .collect();
    let account_home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|value| value.is_absolute())
        .ok_or_else(|| {
            IvyError::new(
                "unsupported_runtime",
                "The executing account's home directory is unavailable.",
            )
        })?;
    Ok(NativeHomeInputs {
        cwd: paths.cwd.clone(),
        paths,
        user_home: environment.get(&JsString::from("HOME")).cloned(),
        account_home: node_path::from_native(&account_home),
        environment,
    })
}

/// Native display ordering. Windows uses its installed locale data; the Linux
/// path uses Unicode text ordering and needs no separately packaged runtime.
pub fn compare_names(left: &JsString, right: &JsString, english: bool) -> Ordering {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::CompareStringEx;
        let locale = [
            b'e' as u16,
            b'n' as u16,
            b'-' as u16,
            b'U' as u16,
            b'S' as u16,
            0,
        ];
        if let (Ok(left_len), Ok(right_len)) =
            (i32::try_from(left.0.len()), i32::try_from(right.0.len()))
        {
            // All buffers remain alive for the synchronous OS call.
            let result = unsafe {
                CompareStringEx(
                    if english {
                        locale.as_ptr()
                    } else {
                        std::ptr::null()
                    },
                    0,
                    left.0.as_ptr(),
                    left_len,
                    right.0.as_ptr(),
                    right_len,
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                )
            };
            match result {
                1 => return Ordering::Less,
                2 => return Ordering::Equal,
                3 => return Ordering::Greater,
                _ => {}
            }
        }
    }
    #[cfg(not(windows))]
    let _ = english;
    let left_text = String::from_utf16_lossy(&left.0);
    let right_text = String::from_utf16_lossy(&right.0);
    left_text
        .to_lowercase()
        .cmp(&right_text.to_lowercase())
        .then_with(|| left_text.cmp(&right_text))
}

pub fn canonical_base64(value: &JsString) -> Option<Vec<u8>> {
    let text = value.to_utf8().ok()?;
    let bytes = STANDARD.decode(text.as_bytes()).ok()?;
    (STANDARD.encode(&bytes) == text).then_some(bytes)
}

pub fn form_query(values: &[(JsString, JsString)]) -> IvyResult<JsString> {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in values {
        query.append_pair(
            &String::from_utf16_lossy(&key.0),
            &String::from_utf16_lossy(&value.0),
        );
    }
    Ok(query.finish().into())
}

pub fn native_bindings(config: &InstanceConfig) -> IvyResult<AgentRuntimeBindings> {
    let value = config.json();
    let url = value.required("publicBaseUrl")?.as_str()?;
    let credential = value.required("credential")?.as_str()?;
    Ok(AgentRuntimeBindings {
        bootstrap: Arc::new(HiveClient::new(
            &url,
            ClientOptions {
                credential: Some(credential),
                ..Default::default()
            },
        )?),
        home: home_inputs()?,
        english_hint_comparator: Arc::new(|left, right| compare_names(left, right, true)),
        skills_comparator: Arc::new(|left, right| compare_names(left, right, false)),
        directory_comparator: Arc::new(|left, right| compare_names(left, right, false)),
        canonical_base64: Arc::new(canonical_base64),
        form_query: Arc::new(form_query),
    })
}
