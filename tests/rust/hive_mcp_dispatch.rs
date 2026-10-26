use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{mcp, mcp_dispatch as dispatch, mcp_request as request};
use std::cell::RefCell;

// The only language operation reached by this single ASCII fixture binding is
// the first-letter title fallback. This does not qualify a language runtime.
pub(crate) struct Language;
impl ivy_hive::retention::Runtime for Language {
    fn parse_date(&self, _: &JsString) -> f64 {
        panic!("No dates")
    }
    fn iso_date(&self, _: f64) -> IvyResult<JsString> {
        panic!("No dates")
    }
    fn compare_contract_keys(&self, _: &JsString, _: &JsString) -> std::cmp::Ordering {
        panic!("No sorting")
    }
}
impl ivy_hive::discovery::Runtime for Language {
    fn lowercase(&self, _: &JsString) -> IvyResult<JsString> {
        panic!("No case folding")
    }
    fn word_character(&self, _: u32) -> bool {
        panic!("No word matching")
    }
    fn compare_english(&self, _: &JsString, _: &JsString) -> std::cmp::Ordering {
        panic!("No sorting")
    }
}
impl mcp::Runtime for Language {
    fn uppercase(&self, value: &JsString) -> IvyResult<JsString> {
        assert_eq!(value, &JsString::from("f"));
        Ok("F".into())
    }
}
pub(crate) fn binding() -> IvyJson {
    json(
        r#"{"qualifiedName":"fixture.read","definition":{"namespace":"fixture","name":"read","interfaceVersion":"1.0.0","description":"Synthetic tool description.","discovery":{"mcp":{"name":"fixture_read","surface":"ivy"}},"inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false},"outputSchema":{"type":"string"},"annotations":{"readOnlyHint":true}},"definitionHash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","serviceName":"sample","provider":{"serviceNodeId":"node","hostId":"host-node","available":true},"guideMarkdown":"Synthetic guide."}"#,
    )
}
struct Storage<'a>(&'a RefCell<Vec<IvyJson>>);
impl request::Storage for Storage<'_> {
    async fn request(&mut self, message: IvyJson) -> IvyResult<IvyJson> {
        let action = message.get("action").unwrap().as_str().unwrap();
        let mut effect = obj([(
            "kind",
            match action.as_str() {
                "mcp.catalog" => "catalog",
                "mcp.cursor.read" => "cursor-read",
                "mcp.cursor.create" => "cursor-create",
                _ => panic!("Unexpected action"),
            }
            .into(),
        )]);
        for (from, to) in [
            ("name", "tool"),
            ("identity", "identity"),
            ("offset", "offset"),
            ("cursor", "cursor"),
        ] {
            if let Some(value) = message.get(from) {
                effect.insert(to, value.clone());
            }
        }
        self.0.borrow_mut().push(effect);
        match action.as_str() {
            "mcp.catalog" => Ok(IvyJson::Array(
                if message
                    .get("name")
                    .is_none_or(|v| v == &"fixture_read".into())
                {
                    vec![binding()]
                } else {
                    vec![]
                },
            )),
            "mcp.cursor.read" if message.get("cursor") == Some(&"end".into()) => {
                Ok(obj([("offset", number(1))]))
            }
            "mcp.cursor.read" => Err(IvyError::new("invalid_cursor", "Invalid fixture cursor.")),
            "mcp.cursor.create" => Ok(obj([("cursor", "fixture-cursor".into())])),
            _ => panic!("Unexpected action"),
        }
    }
}
struct Invocation<'a> {
    effects: &'a RefCell<Vec<IvyJson>>,
    row: &'a IvyJson,
}
impl request::Invocation for Invocation<'_> {
    fn new_id(&mut self) -> JsString {
        "native-request".into()
    }
    fn now_ms(&mut self) -> f64 {
        0.0
    }
    async fn invoke(&mut self, envelope: IvyJson) -> request::Result<IvyJson> {
        self.effects.borrow_mut().push(obj([
            ("kind", "invoke".into()),
            ("frame", envelope.clone()),
        ]));
        // Exercise an actual Pending -> Ready edge inside the native dispatch.
        tokio::task::yield_now().await;
        Ok(
            if envelope.get("method") == Some(&"discovery.instructions".into()) {
                obj([("instructions", "Dynamic fixture instructions.".into())])
            } else {
                self.row
                    .get("reply")
                    .cloned()
                    .unwrap_or_else(|| "answer".into())
            },
        )
    }
    fn observe(&mut self, _: request::Observation) -> request::Result<()> {
        panic!("No observation sink")
    }
    fn observing(&self) -> bool {
        false
    }
}
struct Application<'a> {
    options: request::Options,
    info: IvyJson,
    instructions: IvyJson,
    row: &'a IvyJson,
    effects: RefCell<Vec<IvyJson>>,
}
impl dispatch::Application for Application<'_> {
    fn info(&self) -> &IvyJson {
        &self.info
    }
    fn instructions(&self) -> &IvyJson {
        &self.instructions
    }
    async fn list_tools(
        &self,
        cursor: Option<&JsString>,
    ) -> Result<IvyJson, request::ProtocolError> {
        request::list_tools(
            &mut Storage(&self.effects),
            &self.options,
            &self.info,
            cursor,
            &Language,
        )
        .await
    }
    async fn call_tool(&self, name: &JsString, args: Option<&IvyJson>) -> request::Result<IvyJson> {
        request::call_tool(
            &mut Storage(&self.effects),
            &self.options,
            self.row.get("body").unwrap(),
            name,
            args,
            &Language,
            &mut Invocation {
                effects: &self.effects,
                row: self.row,
            },
        )
        .await
    }
}
async fn execute(row: &IvyJson) -> IvyJson {
    let development = row.get("path") == Some(&"/mcp-dev".into());
    let options = request::Options {
        surface: if development { "ivy_dev" } else { "ivy" }.into(),
        version: "1.2.3".into(),
        public_base_url: "https://EXAMPLE.invalid:443/ivy/".into(),
        resource_metadata_url: if development {
            "https://example.invalid/ivy/.well-known/oauth-protected-resource/mcp-dev"
        } else {
            "https://example.invalid/ivy/.well-known/oauth-protected-resource"
        }
        .into(),
    };
    let effects = RefCell::new(vec![]);
    let instructions = request::instructions(
        &options,
        row.get("body").unwrap(),
        &mut Invocation {
            effects: &effects,
            row,
        },
    )
    .await
    .unwrap();
    let app = Application {
        info: request::server_info(&options).unwrap(),
        options,
        instructions,
        row,
        effects,
    };
    let reply = dispatch::reply(&super::mcp_inbound::at(row), &app)
        .await
        .unwrap();
    obj([
        ("name", row.get("name").unwrap().clone()),
        (
            "wire",
            obj([
                ("status", number(reply.status.into())),
                ("headers", reply.headers),
                ("bodyBase64", STANDARD.encode(reply.body.as_bytes()).into()),
            ]),
        ),
        ("effects", IvyJson::Array(app.effects.into_inner())),
    ])
}
fn fresh_cases(reference: &IvyJson) -> Vec<IvyJson> {
    let base = reference.get("cases").unwrap().as_array().unwrap();
    let mut cases = vec![];
    for (era, index) in [("legacy", 28), ("modern", 31)] {
        for (i, value) in [
            IvyJson::Null,
            true.into(),
            number(123),
            json("[]"),
            json("{}"),
            "state".into(),
            "".into(),
        ]
        .into_iter()
        .enumerate()
        {
            let mut row = base[index].clone();
            super::mcp_inbound::set(&mut row, &["body", "params", "requestState"], value);
            row.insert("name", format!("{era}-state-{i}").into());
            cases.push(row);
        }
        for (tag, value) in [
            (
                "png",
                json(r#"{"mimeType":"image/png","encoding":"base64","data":"iVBORw0KGgo="}"#),
            ),
            (
                "object",
                json(r#"{"10":true,"2":false,"nested":[null,1.25],"\ud800":"value"}"#),
            ),
        ] {
            let mut row = base[index].clone();
            row.insert("reply", value);
            row.insert("name", format!("{era}-{tag}").into());
            cases.push(row);
        }
        let mut row = base[index].clone();
        super::mcp_inbound::set(&mut row, &["body", "params", "name"], "".into());
        if era == "modern" {
            super::mcp_inbound::set(&mut row, &["headers", "mcp-name"], "".into());
        }
        row.insert("name", format!("{era}-empty-tool-name").into());
        cases.push(row);
    }
    for index in [7, 8, 12, 13, 28, 31] {
        let mut row = base[index].clone();
        row.get_mut("body").unwrap().remove("params");
        row.insert("name", format!("missing-params-{index}").into());
        cases.push(row);
    }
    cases
}
fn strip_ids(effects: &IvyJson) -> IvyJson {
    let mut effects = effects.clone();
    let IvyJson::Array(items) = &mut effects else {
        panic!("Expected effects")
    };
    for item in items {
        if let Some(frame) = item.get_mut("frame") {
            frame.remove("id");
        }
    }
    effects
}
fn original_storage_effects(source: &IvyJson, name: &IvyJson) -> IvyJson {
    let catalog: Vec<_> = source
        .get("catalogRequests")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row.get("name") == Some(name))
        .collect();
    let mut index = 0;
    let effects = source
        .get("effects")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row.get("name") == Some(name))
        .map(|row| {
            if row.get("kind") == Some(&"catalog".into()) {
                let request = catalog[index].get("payload").unwrap();
                index += 1;
                assert_eq!(request.get("action"), Some(&"mcp.catalog".into()));
                let mut effect = obj([("kind", "catalog".into())]);
                if let Some(name) = request.get("name") {
                    effect.insert("tool", name.clone());
                }
                effect
            } else {
                let mut effect = row.clone();
                effect.remove("name");
                effect
            }
        })
        .collect();
    assert_eq!(index, catalog.len());
    IvyJson::Array(effects)
}
pub(super) fn reference() -> IvyJson {
    let reference = super::mcp_inbound::reference();
    let cached = reference.get("cases").unwrap().as_array().unwrap();
    let cases: Vec<_> = cached
        .iter()
        .filter(|r| r.get("method").is_none())
        .cloned()
        .chain(fresh_cases(&reference))
        .collect();
    let source = super::mcp_protocol::reference(&obj([
        ("mode", "baseline".into()),
        ("client", false.into()),
        ("recordEffects", true.into()),
        ("recordStorageEffects", true.into()),
        ("cases", IvyJson::Array(cases.clone())),
    ]));
    obj([("cases", IvyJson::Array(cases)), ("source", source)])
}
#[tokio::test(flavor = "current_thread")]
async fn registered_dispatch_matches_original_http_and_application_effects() {
    let reference = super::mcp_inbound::reference();
    let comparison = self::reference();
    let cases = comparison.get("cases").unwrap().as_array().unwrap();
    let source = comparison.get("source").unwrap();
    let mut native = vec![];
    for row in cases {
        native.push(execute(row).await);
    }
    let wires = source.get("wire").unwrap().as_array().unwrap();
    assert_eq!(wires.len(), cases.len());
    for (i, row) in native.iter().enumerate() {
        let original = &wires[i];
        let wire = obj([
            ("status", original.get("status").unwrap().clone()),
            ("headers", original.get("headers").unwrap().clone()),
            ("bodyBase64", original.get("bodyBase64").unwrap().clone()),
        ]);
        assert_eq!(
            row.get("wire"),
            Some(&wire),
            "Wire differs for {}",
            row.get("name").unwrap().as_str().unwrap()
        );
        let original_effects = original_storage_effects(&source, row.get("name").unwrap());
        assert_eq!(
            strip_ids(row.get("effects").unwrap()),
            strip_ids(&original_effects),
            "Storage/application effects differ for {}",
            row.get("name").unwrap().as_str().unwrap()
        );
        if i < 55 {
            let cached_wire = &reference.get("wire").unwrap().as_array().unwrap()[i];
            assert_eq!(
                original.get("bodyBase64"),
                cached_wire.get("bodyBase64"),
                "Cached original body changed"
            );
        }
    }
}
