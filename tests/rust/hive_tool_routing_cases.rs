// Ingress composition cases. The shared contracts corpus owns exhaustive field
// boundaries; these exercise the actual admission/dispatcher boundary instead.
use serde_json::{Value, json};

pub fn requests() -> Vec<(String, Value)> {
    let mut cases = vec![];
    for method in ["tools.call", "discovery.call"] {
        let base = json!({"jsonrpc":"2.0","id":"call","method":method,"params":{"qualifiedName":"sample.read","expectedDefinitionHash":format!("sha256:{}","a".repeat(64)),"arguments":{},"serviceName":"sample","serviceNodeId":"node"}});
        cases.push((format!("{method}-valid"), base.clone()));
        for (name, params) in [
            ("null-params", Value::Null),
            ("array-params", json!([])),
            ("missing-fields", json!({})),
        ] {
            let mut request = base.clone();
            request["params"] = params;
            cases.push((format!("{method}-{name}"), request));
        }
        for (key, value) in [
            ("qualifiedName", json!("x".repeat(257))),
            (
                "expectedDefinitionHash",
                json!(format!("sha256:{}\n", "a".repeat(64))),
            ),
            ("operationId", Value::Null),
            ("expectedCallerPrincipalId", Value::Null),
            ("serviceNodeId", Value::Null),
            ("serviceName", json!("Sample")),
            ("extra", json!(true)),
            ("resourceRef", Value::Null),
            ("resourceRef", json!([])),
            (
                "resourceRef",
                json!({"serviceNodeId":"node","namespace":"sample","kind":"item","nativeId":"x".repeat(1025)}),
            ),
            ("hostId", json!("host")),
        ] {
            let mut request = base.clone();
            request["params"][key] = value;
            cases.push((format!("{method}-{key}-{}", cases.len()), request));
        }
        for arguments in [
            Value::Null,
            json!(false),
            json!(0),
            json!("text"),
            json!([1]),
            json!({"nested":[null,true]}),
        ] {
            let mut request = base.clone();
            request["params"]["arguments"] = arguments;
            cases.push((format!("{method}-arguments-{}", cases.len()), request));
        }
        for (key, value) in [
            ("jsonrpc", json!("1.0")),
            ("id", Value::Null),
            ("extra", json!(true)),
        ] {
            let mut request = base.clone();
            request[key] = value;
            request["params"]["extra"] = json!(true);
            cases.push((format!("{method}-envelope-{key}-first"), request));
        }
        let mut missing = base.clone();
        missing["params"]
            .as_object_mut()
            .unwrap()
            .remove("arguments");
        cases.push((format!("{method}-absent-arguments"), missing));
        let mut competing = base.clone();
        competing["params"]["qualifiedName"] = json!("");
        competing["params"]["resourceRef"] = Value::Null;
        cases.push((format!("{method}-routing-before-reference"), competing));
        if method == "discovery.call" {
            let mut missing = base.clone();
            missing["params"]
                .as_object_mut()
                .unwrap()
                .remove("serviceName");
            cases.push(("discovery-missing-service".into(), missing));
        }
    }
    cases
}
