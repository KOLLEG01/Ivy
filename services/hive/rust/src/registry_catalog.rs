//! One immutable tool catalog shared by registry readers and routing handoffs.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::{collections::HashMap, mem::size_of, ops::Range, sync::Arc};

const CACHE_BYTES: usize = 32 * 1024 * 1024;
const ENTRY_BYTES: usize = 8 * 1024 * 1024;
const CACHE_ENTRIES: usize = 128;

#[derive(Debug, PartialEq)]
pub struct ToolBinding {
    pub qualified_name: JsString,
    pub definition_hash: JsString,
    pub definition: IvyJson,
}
impl ToolBinding {
    /// Materialize the external binding only at a response boundary.
    pub fn to_wire(&self) -> IvyJson {
        IvyJson::object([
            ("qualifiedName", self.qualified_name.clone().into()),
            ("definition", self.definition.clone()),
            ("definitionHash", self.definition_hash.clone().into()),
        ])
    }
    fn heap_bytes(&self) -> usize {
        self.qualified_name.0.capacity() * 2
            + self.definition_hash.0.capacity() * 2
            + self.definition.retained_bytes() - size_of::<IvyJson>()
    }
}

#[derive(Debug, PartialEq)]
pub struct Namespace {
    pub name: JsString,
    pub guide_markdown: Option<JsString>,
    pub tools: Range<usize>,
}
#[derive(Debug, PartialEq)]
pub struct Catalog {
    pub namespaces: Vec<Namespace>,
    pub tools: Vec<ToolBinding>,
    // Notification definitions contain external payload schemas.
    pub notifications: Vec<IvyJson>,
}
impl Catalog {
    pub(crate) fn new(
        registry: &IvyJson,
        mut bindings: HashMap<JsString, Vec<ToolBinding>>,
    ) -> IvyResult<Self> {
        let mut namespaces = vec![];
        let mut tools = vec![];
        let mut notifications = vec![];
        for ns in registry.required("namespaces")?.as_array().ok_or_else(IvyError::internal)? {
            let name = ns.required("namespace")?.as_js_string().ok_or_else(IvyError::internal)?.clone();
            if let Some(values) = ns.get("notifications").filter(|v| !v.is_null()) {
                for notification in values.as_array().ok_or_else(IvyError::internal)? {
                    let mut value = IvyJson::object([("namespace", name.clone().into())]);
                    for (key, child) in notification.as_object().ok_or_else(IvyError::internal)? {
                        value.insert(key.clone(), child.clone());
                    }
                    notifications.push(value);
                }
            }
            let start = tools.len();
            tools.extend(bindings.remove(&name).unwrap_or_default());
            namespaces.push(Namespace {
                tools: start..tools.len(),
                name,
                guide_markdown: ns.get("guideMarkdown").and_then(IvyJson::as_js_string).cloned(),
            });
        }
        Ok(Self { namespaces, tools, notifications })
    }
    pub fn bindings(&self, namespace: &Namespace) -> &[ToolBinding] {
        &self.tools[namespace.tools.clone()]
    }
    pub(crate) fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.namespaces.capacity() * size_of::<Namespace>()
            + self.namespaces.iter().map(|ns| {
                ns.name.0.capacity() * 2
                    + ns.guide_markdown.as_ref().map_or(0, |v| v.0.capacity() * 2)
            }).sum::<usize>()
            + self.tools.capacity() * size_of::<ToolBinding>()
            + self.tools.iter().map(ToolBinding::heap_bytes).sum::<usize>()
            + self.notifications.capacity() * size_of::<IvyJson>()
            + self.notifications.iter().map(|v| v.retained_bytes() - size_of::<IvyJson>()).sum::<usize>()
    }
}

#[derive(Default)]
pub(crate) struct CatalogCache {
    entries: Vec<(JsString, Arc<Catalog>, usize)>,
    bytes: usize,
}
impl CatalogCache {
    pub(crate) fn get(&mut self, id: &JsString) -> Option<Arc<Catalog>> {
        let index = self.entries.iter().position(|v| &v.0 == id)?;
        let entry = self.entries.remove(index);
        let result = entry.1.clone();
        self.entries.push(entry);
        Some(result)
    }
    fn remove(&mut self, index: usize) {
        self.bytes -= self.entries.remove(index).2;
    }
    pub(crate) fn insert(&mut self, id: JsString, catalog: Catalog) -> Arc<Catalog> {
        if let Some(index) = self.entries.iter().position(|v| v.0 == id) {
            self.remove(index);
        }
        let bytes = catalog.retained_bytes() + id.0.capacity() * 2 + 2 * size_of::<usize>();
        let result = Arc::new(catalog);
        if bytes + size_of::<(JsString, Arc<Catalog>, usize)>() > ENTRY_BYTES {
            return result;
        }
        if self.entries.len() == CACHE_ENTRIES {
            self.remove(0);
        }
        self.entries.push((id, result.clone(), bytes));
        self.bytes += bytes;
        while self.retained_bytes() > CACHE_BYTES {
            self.remove(0);
        }
        result
    }
    fn retained_bytes(&self) -> usize {
        size_of::<Self>() + self.bytes
            + self.entries.capacity() * size_of::<(JsString, Arc<Catalog>, usize)>()
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_registry_catalog.rs"]
mod tests;
