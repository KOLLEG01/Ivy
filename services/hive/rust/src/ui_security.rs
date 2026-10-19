//! Content policy for published assets, including isolated script sandboxes.
use ivy_contracts::JsString;

pub fn content_policy(path: &JsString) -> String {
    if path
        .0
        .ends_with(&".sandbox.html".encode_utf16().collect::<Vec<_>>())
    {
        return "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'".into();
    }
    let mut policy = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; object-src 'none'; form-action *; worker-src 'self'; manifest-src 'self'".to_owned();
    if !path.0.is_empty() {
        policy.push_str("; frame-src 'self'");
    }
    policy
}
