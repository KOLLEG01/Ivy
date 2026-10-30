use super::*;
use ivy_contracts::schema::{admit_schema, scalar_types};

fn unique(items: &[IvyJson], key: impl Fn(&IvyJson) -> IvyResult<JsString>) -> IvyResult<bool> {
    let mut keys = HashSet::new();
    for value in items {
        if !keys.insert(key(value)?) {
            return Ok(false);
        }
    }
    Ok(true)
}
fn version_identity(value: &IvyJson, name: &str) -> IvyResult<JsString> {
    Ok(join([
        string(value, name)?.clone(),
        "@".into(),
        string(value, "version")?.clone(),
    ]))
}
fn mcp_name(tool: &IvyJson) -> Option<&JsString> {
    tool.get("discovery")
        .and_then(|v| v.get("mcp"))
        .and_then(|v| v.get("name"))
        .and_then(IvyJson::as_js_string)
        .filter(|v| !v.0.is_empty())
}
impl Registry {
    pub fn sync(
        &mut self,
        store: &mut Store,
        id: &JsString,
        generation: Option<f64>,
        parameters: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let node = self.node(store, id, runtime, now)?;
        let node_id = string(&node, "serviceNodeId")?.clone();
        let service = string(&node, "serviceName")?.clone();
        let contacted = iso(runtime, now)?;
        self.record_contact(node_id.clone(), contacted);
        self.contacts
            .get_mut(&node_id)
            .unwrap()
            .contracts_acknowledged = false;
        let result=store.transaction(|store|{
            let mut next_bindings = HashMap::new();
            validate_shared("RegistrySync",parameters)?;
            let namespaces=array(parameters,"namespaces")?;
            require(unique(namespaces,|ns|Ok(string(ns,"namespace")?.clone()))?,"registry_invalid","Duplicate namespace.")?;
            for contract in array(parameters,"contracts")?{
                crate::objects::register(store,contract,&IvyJson::object([("kind","service".into()),("serviceName",IvyJson::String(service.clone()))]))?;
            }
            self.check_requirements(store,array(parameters,"requiredContracts")?,false)?;
            let mut names:HashMap<JsString,JsString>=HashMap::new();
            let prefix=parameters.get("mcpPrefix").filter(|v|!v.is_null()).and_then(IvyJson::as_js_string).cloned().unwrap_or_else(||JsString(service.0.iter().map(|u|if *u==45{95}else{*u}).collect()));
            require(prefix!=JsString::from("hive")&&prefix!=JsString::from("wiki"),"registry_invalid","Hive and Wiki MCP prefixes are reserved.")?;
            let mut other_names=HashSet::new();
            for owner in self.nodes(store,runtime,now)?{
                if owner.get("serviceName").and_then(IvyJson::as_js_string)==Some(&service){continue;}
                if let Some(registry)=self.get_registry(store,string(&owner,"serviceNodeId")?)?.filter(|v|!v.is_null()){
                    for ns in array(&registry,"namespaces")?{for tool in array(ns,"tools")?{require_object(tool)?;if let Some(name)=mcp_name(tool){other_names.insert(name.clone());}}}
                }
            }
            for ns in namespaces{
                let namespace=string(ns,"namespace")?;
                require(namespace!=&JsString::from("hive"),"registry_invalid","The Hive namespace is reserved.")?;
                let declared_groups=optional_array(ns,"discoveryGroups")?;
                let groups=declared_groups.iter().map(|g|Ok(string(g,"group")?.clone())).collect::<IvyResult<HashSet<_>>>()?;
                let group_prefix=join([namespace.clone(),"/".into()]);
                require(groups.len()==declared_groups.len()&&groups.iter().all(|g|g==namespace||g.0.starts_with(&group_prefix.0)),"registry_invalid","Discovery groups must be unique children of their namespace.")?;
                let tools=array(ns,"tools")?;
                require(unique(tools,|t|Ok(string(t,"name")?.clone()))?,"registry_invalid","Duplicate tool name.")?;
                let versions=tools.iter().map(|t|Ok(string(t,"interfaceVersion")?.clone())).collect::<IvyResult<HashSet<_>>>()?;
                require(versions.len()<=1,"registry_invalid","A namespace exposes one interface version.")?;
                let mut bindings=vec![];
                for tool in tools{
                    if let Some(name)=mcp_name(tool){
                        let starts=join([prefix.clone(),"_".into()]);
                        require(name.0.starts_with(&starts.0)&&!["hive_","wiki_"].iter().any(|p|name.0.starts_with(&p.encode_utf16().collect::<Vec<_>>()))&&names.get(name).is_none_or(|ns|ns==namespace)&&!other_names.contains(name),"registry_invalid","MCP names must be unique and use the service-declared MCP prefix. Hive and Wiki prefixes are reserved.")?;
                        names.insert(name.clone(),namespace.clone());
                    }
                    require(tool.get("namespace").and_then(IvyJson::as_js_string)==Some(namespace),"registry_invalid","Tool namespace does not match its container.")?;
                    if let Some(group)=tool.get("discovery").and_then(|v|v.get("group")).filter(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty())){require(group.as_js_string().is_some_and(|g|groups.contains(g)),"registry_invalid","A Tool discovery group must be declared by its namespace.")?;}
                    if let Some(method)=tool.get("nativeMethod"){require(Some(method)==tool.get("name"),"registry_invalid","Native methods must preserve their exact names.")?;}
                    admit_schema(field(tool,"inputSchema")?)?;admit_schema(field(tool,"outputSchema")?)?;
                    bindings.extend(tool_bindings(std::slice::from_ref(tool))?);
                }
                next_bindings.insert(namespace.clone(),bindings);
                let notifications=optional_array(ns,"notifications")?;
                require(unique(notifications,|n|version_identity(n,"name"))?,"registry_invalid","Duplicate transient notification version.")?;
                for notification in notifications{admit_schema(field(notification,"payloadSchema")?)?;}
                let topics=array(ns,"topics")?;
                require(unique(topics,|t|version_identity(t,"topic"))?,"registry_invalid","Duplicate topic version.")?;
                for topic in topics{
                    require(string(topic,"topic")?.0.starts_with(&join([namespace.clone(),".".into()]).0),"registry_invalid","Topic belongs to another namespace.")?;
                    require(unique(array(topic,"eventKinds")?,|k|Ok(string(k,"kind")?.clone()))?,"registry_invalid","Duplicate event kind.")?;
                    crate::events::register_topic(store,topic,&join(["service:".into(),service.clone()]))?;
                }
                let kinds=array(ns,"inventoryKinds")?;
                require(unique(kinds,|k|version_identity(k,"kind"))?,"registry_invalid","Duplicate inventory kind version.")?;
                for kind in kinds{
                    let schema=field(kind,"summarySchema")?;store.validators().compile(schema)?;
                    for pointer in optional_array(kind,"searchPointers")?{
                        require(scalar_types(schema,pointer.as_js_string().ok_or_else(IvyError::internal)?)?.iter().any(|v|v=="string"),"registry_invalid","Inventory search paths must select strings.")?;
                    }
                    if let Some(pointer)=kind.get("archivedPointer").filter(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty())){require(scalar_types(schema,pointer.as_js_string().ok_or_else(IvyError::internal)?)?.iter().any(|v|v=="boolean"),"registry_invalid","Inventory archive paths must select booleans.")?;}
                    if let Some(pointer)=kind.get("recencyPointer").filter(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty())){require(scalar_types(schema,pointer.as_js_string().ok_or_else(IvyError::internal)?)?.iter().any(|v|["string","number","integer"].contains(&v.as_str())),"registry_invalid","Inventory recency paths must select strings or numbers.")?;}
                    let prior=sql(store,store.connection().query_row("SELECT definition FROM inventory_schemas WHERE namespace=? AND kind=? AND version=?",params![text(namespace),text(string(kind,"kind")?),text(string(kind,"version")?)],|r|r.get::<_,String>(0)).optional())?;
                    let candidate=encoded(kind)?;
                    require(prior.is_none_or(|v|v==candidate),"registry_invalid","Inventory schema version is immutable.")?;
                    sql(store,store.connection().execute("INSERT OR IGNORE INTO inventory_schemas VALUES (?,?,?,?)",params![text(namespace),text(string(kind,"kind")?),text(string(kind,"version")?),candidate]))?;
                }
            }
            let catalog = Catalog::new(parameters, next_bindings)?;
            let at=iso(runtime,now)?;self.record_contact(node_id.clone(),at.clone());
            sql(store,store.connection().execute("INSERT INTO registries VALUES (?,?) ON CONFLICT(node_id) DO UPDATE SET registry_json=excluded.registry_json",params![text(&node_id),encoded(parameters)?]))?;
            let mut updated=node.clone();updated.insert("synced",true.into());updated.insert("lastSuccessfulSyncAt",IvyJson::String(at.clone()));updated.insert("lastContactAt",IvyJson::String(at.clone()));self.save(store,&updated,true)?;
            self.diagnostic(store,&registry_diagnostic(&node_id,&at,true),runtime,now)?;
            let mut result=IvyJson::object(std::iter::empty::<(&str,IvyJson)>());
            if let Some(generation)=generation {result.insert("generation",IvyJson::Number(generation));}
            result.insert("syncedAt",IvyJson::String(at));
            Ok((result,catalog))
        });
        match result {
            Ok((result,catalog)) => {
                self.catalogs.insert(node_id, catalog);
                Ok(result)
            }
            Err(error) => {
                let at = iso(runtime, now)?;
                self.record_contact(node_id.clone(), at.clone());
                let mut updated = node;
                updated.insert("synced", false.into());
                updated.insert("ready", false.into());
                updated.insert("lastFailedSyncAt", IvyJson::String(at.clone()));
                self.save(store, &updated, true)?;
                self.diagnostic(
                    store,
                    &registry_diagnostic(&node_id, &at, false),
                    runtime,
                    now,
                )?;
                // SQL/JSON faults originate as native errors in the original try
                // block; named Ivy admission failures retain their exact errors.
                Err(
                    if error.code == "internal_error"
                        && error.outcome == ivy_contracts::Outcome::Unknown
                    {
                        IvyError::new(
                            "registry_invalid",
                            "Registry candidate could not be applied.",
                        )
                    } else {
                        error
                    },
                )
            }
        }
    }
}
fn registry_diagnostic(id: &JsString, at: &JsString, resolved: bool) -> IvyJson {
    IvyJson::object([
        ("code", "registry_invalid".into()),
        (
            "resource",
            IvyJson::object([("serviceNodeId", IvyJson::String(id.clone()))]),
        ),
        ("severity", "error".into()),
        ("source", "hive".into()),
        ("firstObservedAt", IvyJson::String(at.clone())),
        ("lastObservedAt", IvyJson::String(at.clone())),
        (
            "status",
            if resolved { "resolved" } else { "current" }.into(),
        ),
        (
            "message",
            if resolved {
                "Registry is synchronized."
            } else {
                "Registry candidate rejected; the previous catalog remains cached."
            }
            .into(),
        ),
    ])
}
