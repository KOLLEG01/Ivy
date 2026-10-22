//! Direct server methods after ingress freshness/admission. Both ingress paths
//! bind this dispatcher to the same peer, routing and package owners.
use crate::{
    discovery::CoreCalls,
    packages::PackageRegistry,
    server_peers::{Peer, Peers},
    server_rpc::{Clock, Reply, require_fresh},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::{validate_input, validate_output},
    validation::{validate_request, validate_tool_routing},
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Options {
    pub validate_messages: bool,
    pub package_publisher_principal_ids: Vec<JsString>,
}
struct Parts {
    core: Mutex<CoreCalls>,
    packages: Option<PackageRegistry>,
    now: Clock,
    options: Options,
}
#[derive(Clone)]
pub struct Dispatcher(Arc<Parts>);
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
impl Dispatcher {
    pub fn new(now: Clock, packages: Option<PackageRegistry>, options: Options) -> Self {
        Self(Arc::new(Parts {
            core: Mutex::new(CoreCalls::default()),
            packages,
            now,
            options,
        }))
    }
    fn checked(&self, method: &'static str, work: Reply) -> Reply {
        let validate = self.0.options.validate_messages;
        Box::pin(async move {
            let value = work.await?;
            if validate {
                validate_output(method, &value)?;
            }
            Ok(value)
        })
    }
    fn packages(&self) -> IvyResult<&PackageRegistry> {
        self.0.packages.as_ref().ok_or_else(|| {
            IvyError::new(
                "service_unavailable",
                "Hive package storage is not configured.",
            )
        })
    }
    /// Invocation::direct and Peers::message check freshness before entering.
    /// Recursive core calls repeat that check at the original recursive boundary.
    pub fn dispatch(
        &self,
        peers: &Peers,
        context: &IvyJson,
        request: &IvyJson,
        peer: Option<&Peer>,
    ) -> IvyResult<Option<Reply>> {
        let Some(method) = request.get("method").and_then(IvyJson::as_js_string) else {
            return Ok(None);
        };
        let method = match method.to_utf8().as_deref() {
            Ok("tools.call") => "tools.call",
            Ok("discovery.call") => "discovery.call",
            Ok("notifications.subscribe") => "notifications.subscribe",
            Ok("packages.catalog") => "packages.catalog",
            Ok("packages.authorizeUpload") => "packages.authorizeUpload",
            _ => return Ok(None),
        };
        validate_request(request)?;
        let params = request.required("params")?;
        match method {
            "notifications.subscribe" => return peers.subscribe(peer, context, params).map(Some),
            "packages.catalog" => {
                validate_input(method, params)?;
                let packages = self.packages()?;
                let work = packages.catalog_snapshot(
                    params.required("after")?,
                    params.get("includeUis") != Some(&false.into()),
                );
                return Ok(Some(self.checked(method, work)));
            }
            "packages.authorizeUpload" => {
                validate_input(method, params)?;
                let packages = self.packages()?;
                let principal = context
                    .required("principalId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?;
                require(
                    self.0
                        .options
                        .package_publisher_principal_ids
                        .contains(principal),
                    "forbidden",
                    "Caller may not publish Ivy packages.",
                )?;
                return Ok(Some(
                    self.checked(method, packages.authorize(principal, params)),
                ));
            }
            _ => {}
        }
        validate_tool_routing(params, method == "discovery.call")?;
        if method == "discovery.call" {
            if params.get("serviceName") == Some(&"hive".into()) {
                require(
                    params.get("expectedCallerPrincipalId").is_none()
                        || params.get("expectedCallerPrincipalId") == context.get("principalId"),
                    "caller_changed",
                    "Return to the original caller before reconciling or repeating this action.",
                )?;
                let resolved = self.0.core.lock().unwrap().resolve(params)?;
                if ![
                    "notifications.subscribe",
                    "packages.catalog",
                    "packages.authorizeUpload",
                ]
                .iter()
                .any(|method| resolved.get("method") == Some(&(*method).into()))
                {
                    return Ok(None);
                }
                let mut next = IvyJson::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", request.required("id")?.clone()),
                ]);
                next.insert("method", resolved.required("method")?.clone());
                next.insert("params", resolved.required("params")?.clone());
                require_fresh(context, &*self.0.now)?;
                return self.dispatch(peers, context, &next, peer);
            }
            require(
                params
                    .get("serviceNodeId")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|s| !s.0.is_empty()),
                "invalid_arguments",
                "Service calls require the exact described serviceNodeId.",
            )?;
        }
        peers.call(context, params).map(Some)
    }
}
