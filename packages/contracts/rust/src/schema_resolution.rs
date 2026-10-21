//! Resource and pointer resolution before target validator compilation/caching.
use super::{compiler_walk, resources, uri};
use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    schema_lookup::{self, Value},
};
use std::collections::{HashMap, HashSet};

fn invalid() -> IvyError {
    IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.")
}
static ACCEPT: IvyJson = IvyJson::Bool(true);

#[derive(Clone)]
pub struct CompilerTarget<'a> {
    pub schema: &'a IvyJson,
    pub base_id: JsString,
    pub root_schema: &'a IvyJson,
    pub root_base_id: JsString,
}
impl<'a> CompilerTarget<'a> {
    pub(super) fn root(&self) -> Self {
        Self {
            schema: self.root_schema,
            base_id: self.root_base_id.clone(),
            root_schema: self.root_schema,
            root_base_id: self.root_base_id.clone(),
        }
    }
}
enum Entry<'a> {
    Schema(CompilerTarget<'a>),
    Alias(JsString),
    Inherited,
}

/// Fresh registered resources. Resolution returns targets without compiling or
/// caching their validators; callers must finish those stages before use.
pub struct CompilerResources<'a> {
    root: CompilerTarget<'a>,
    refs: HashMap<JsString, Entry<'a>>,
    locals: HashMap<JsString, &'a IvyJson>,
}
enum Step<'a> {
    Resolve(CompilerTarget<'a>, JsString),
    Pointer(CompilerTarget<'a>, Option<JsString>),
    AliasPointer(Option<JsString>),
    FinishPointer(CompilerTarget<'a>),
    Exit((usize, JsString)),
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null | IvyJson::Bool(false) => false,
        IvyJson::Number(n) => *n != 0.0 && !n.is_nan(),
        IvyJson::String(s) => !s.0.is_empty(),
        _ => true,
    }
}
impl<'a> CompilerResources<'a> {
    pub(super) fn new(schema: &'a IvyJson) -> IvyResult<Self> {
        let index = resources::register(schema)?;
        let root = CompilerTarget {
            schema,
            base_id: index.base_id.clone(),
            root_schema: schema,
            root_base_id: index.base_id.clone(),
        };
        let mut refs = HashMap::new();
        for document in resources::meta_documents()? {
            let id = document
                .get("$id")
                .and_then(IvyJson::as_js_string)
                .ok_or_else(IvyError::internal)?
                .clone();
            refs.insert(
                id.clone(),
                Entry::Schema(CompilerTarget {
                    schema: document,
                    base_id: id.clone(),
                    root_schema: document,
                    root_base_id: id,
                }),
            );
        }
        refs.insert(
            "http://json-schema.org/schema".into(),
            Entry::Alias("https://json-schema.org/draft/2020-12/schema".into()),
        );
        let mut locals = HashMap::new();
        for reference in index.references.iter().filter(|r| r.alias) {
            if reference.id.0.first() == Some(&35) {
                locals.insert(
                    reference.id.clone(),
                    index.graph.nodes[reference.node].schema,
                );
            } else {
                refs.insert(
                    reference.id.clone(),
                    Entry::Alias(reference.full_path.clone()),
                );
            }
        }
        if index.registered_root {
            refs.insert(index.base_id, Entry::Schema(root.clone()));
        }
        Ok(Self { root, refs, locals })
    }
    fn lookup(&self, id: &JsString) -> Option<&Entry<'a>> {
        self.refs.get(id).or_else(|| {
            const NAMES: &[&str] = &[
                "constructor",
                "__defineGetter__",
                "__defineSetter__",
                "hasOwnProperty",
                "__lookupGetter__",
                "__lookupSetter__",
                "isPrototypeOf",
                "propertyIsEnumerable",
                "toString",
                "valueOf",
                "__proto__",
                "toLocaleString",
            ];
            NAMES
                .iter()
                .any(|name| id == &JsString::from(*name))
                .then_some(&Entry::Inherited)
        })
    }
    /// Resolve one request as the compiler does before inlineOrCompile. Missing
    /// targets return None; malformed or cyclic resolution returns registry_invalid.
    pub fn resolve(
        &self,
        base_id: &JsString,
        reference: &JsString,
    ) -> IvyResult<Option<CompilerTarget<'a>>> {
        self.resolve_from(&self.root, base_id, reference)
    }
    pub(super) fn submitted_root(&self) -> CompilerTarget<'a> {
        self.root.clone()
    }
    /// Resolve under a previously returned target's root context. A reference
    /// from another resource must not silently use the submitted root instead.
    pub fn resolve_from(
        &self,
        context: &CompilerTarget<'a>,
        base_id: &JsString,
        reference: &JsString,
    ) -> IvyResult<Option<CompilerTarget<'a>>> {
        let absolute = uri::resolve(base_id, &uri::normalize_id(reference))?;
        let mut current = absolute.clone();
        let mut seen = HashSet::new();
        let resolved = loop {
            if !seen.insert(current.clone()) {
                return Err(invalid());
            }
            match self.lookup(&current) {
                Some(Entry::Alias(target)) => current = target.clone(),
                Some(Entry::Schema(target)) => break Some(target.clone()),
                Some(Entry::Inherited) => return Err(invalid()),
                None => break self.resolve_schema(context.root(), current)?,
            }
        };
        Ok(resolved.or_else(|| {
            if !std::ptr::eq(context.root_schema, self.root.schema) {
                return None;
            }
            self.locals.get(&absolute).map(|schema| CompilerTarget {
                schema,
                base_id: base_id.clone(),
                root_schema: self.root.schema,
                root_base_id: self.root.base_id.clone(),
            })
        }))
    }
    fn resolve_schema(
        &self,
        root: CompilerTarget<'a>,
        reference: JsString,
    ) -> IvyResult<Option<CompilerTarget<'a>>> {
        let mut pending = vec![Step::Resolve(root, reference)];
        let mut active = HashSet::new();
        let mut result: Option<CompilerTarget<'a>> = None;
        while let Some(step) = pending.pop() {
            match step {
                Step::Exit(key) => {
                    active.remove(&key);
                }
                Step::AliasPointer(fragment) => {
                    match result
                        .take()
                        .filter(|target| target.schema.as_object().is_some())
                    {
                        Some(target) => pending.push(Step::Pointer(target, fragment)),
                        None => result = None,
                    }
                }
                Step::FinishPointer(fallback) => {
                    let target = result.take().unwrap_or(fallback);
                    if std::ptr::eq(target.schema, target.root_schema) {
                        result = None;
                    } else {
                        result = Some(target);
                    }
                }
                Step::Resolve(root, reference) => {
                    let key = (root.schema as *const _ as usize, reference.clone());
                    if !active.insert(key.clone()) {
                        return Err(invalid());
                    }
                    pending.push(Step::Exit(key));
                    let (path, fragment) = uri::reference_parts(&reference)?;
                    let base = uri::full_path(&uri::normalize_id(&root.base_id))?;
                    result = None;
                    if root
                        .schema
                        .as_object()
                        .is_some_and(|items| !items.is_empty())
                        && path == base
                    {
                        pending.push(Step::Pointer(root, fragment));
                        continue;
                    }
                    let id = uri::normalize_id(&path);
                    match self.lookup(&id) {
                        Some(Entry::Alias(alias)) => {
                            pending.push(Step::AliasPointer(fragment));
                            pending.push(Step::Resolve(root, alias.clone()));
                        }
                        Some(Entry::Schema(target)) if target.schema.as_object().is_some() => {
                            if id == uri::normalize_id(&reference) {
                                let mut base_id = base;
                                if let Some(id) = target.schema.get("$id").filter(|id| truthy(id)) {
                                    base_id = uri::resolve(
                                        &base_id,
                                        &uri::normalize_id(id.as_js_string().ok_or_else(invalid)?),
                                    )?;
                                }
                                result = Some(CompilerTarget {
                                    schema: target.schema,
                                    base_id,
                                    root_schema: root.schema,
                                    root_base_id: root.base_id,
                                });
                            } else {
                                pending.push(Step::Pointer(target.clone(), fragment));
                            }
                        }
                        _ => {}
                    }
                }
                Step::Pointer(target, fragment) => {
                    result = None;
                    let Some(fragment) = fragment.filter(|s| s.0.first() == Some(&47)) else {
                        continue;
                    };
                    let mut selected = Value::Json(target.schema);
                    let mut base_id = target.base_id.clone();
                    let mut missing = false;
                    for raw in fragment.0[1..].split(|c| *c == 47) {
                        if selected.is_boolean() {
                            missing = true;
                            break;
                        }
                        let key = schema_lookup::compiler_pointer_key(raw, true)
                            .map_err(|_| invalid())?;
                        selected = match selected.property(&key).map_err(|_| invalid())? {
                            Some(value) => value,
                            None => {
                                missing = true;
                                Value::Json(&ACCEPT)
                            }
                        };
                        if missing {
                            break;
                        }
                        if let Value::Json(value) = &selected {
                            if matches!(value, IvyJson::Null) {
                                return Err(invalid());
                            }
                            if ![
                                "properties",
                                "patternProperties",
                                "enum",
                                "dependencies",
                                "definitions",
                            ]
                            .iter()
                            .any(|name| raw == JsString::from(*name).0)
                            {
                                if let Some(id) = value.get("$id").filter(|id| truthy(id)) {
                                    base_id = uri::resolve(
                                        &base_id,
                                        &uri::normalize_id(id.as_js_string().ok_or_else(invalid)?),
                                    )?;
                                }
                            }
                        }
                    }
                    if missing {
                        continue;
                    }
                    let schema = selected.schema().map_err(|_| invalid())?.unwrap_or(&ACCEPT);
                    let redirect = if let Some(reference) = schema.get("$ref").filter(|r| truthy(r))
                    {
                        if !compiler_walk::has_rules_but_ref(schema) {
                            Some(uri::resolve(
                                &base_id,
                                &uri::normalize_id(reference.as_js_string().ok_or_else(invalid)?),
                            )?)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let fallback = CompilerTarget {
                        schema,
                        base_id,
                        root_schema: target.root_schema,
                        root_base_id: target.root_base_id,
                    };
                    let root = fallback.root();
                    pending.push(Step::FinishPointer(fallback));
                    if let Some(reference) = redirect {
                        pending.push(Step::Resolve(root, reference));
                    }
                }
            }
        }
        Ok(result)
    }
}
