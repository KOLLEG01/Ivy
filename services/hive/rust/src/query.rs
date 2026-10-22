//! Query planning for admitted ObjectQuery input and immutable object reads.
use crate::{
    paging::{PAGE_BYTES, page},
    store::Store,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
    schema::{pointer_parts, scalar_types},
};
use rusqlite::{
    OptionalExtension,
    types::{Value, ValueRef},
};

fn require(test: bool, code: &str, message: &str) -> IvyResult<()> {
    if test {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn argument<T>(v: Option<T>) -> IvyResult<T> {
    v.ok_or_else(|| IvyError::new("invalid_arguments", "Invalid query arguments."))
}
fn text(v: &JsString) -> String {
    String::from_utf16_lossy(&v.0)
}
fn sql_value(v: &IvyJson) -> IvyResult<Value> {
    Ok(match v {
        IvyJson::Null => Value::Null,
        IvyJson::Bool(v) => Value::Integer(i64::from(*v)),
        IvyJson::Number(v) => Value::Real(*v),
        IvyJson::String(v) => Value::Text(text(v)),
        _ => return Err(IvyError::internal()),
    })
}
fn row_value(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<IvyJson> {
    Ok(match row.get_ref(column)? {
        ValueRef::Null => IvyJson::Null,
        ValueRef::Integer(v) => IvyJson::Number(v as f64),
        ValueRef::Real(v) => IvyJson::Number(v),
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
        ValueRef::Blob(_) => {
            return Err(rusqlite::Error::InvalidColumnType(
                column,
                column.to_string(),
                rusqlite::types::Type::Blob,
            ));
        }
    })
}
fn metadata(name: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match name {
        "id" => ("o.id", &["string"]),
        "parentId" => ("o.parent_id", &["string", "null"]),
        "ownerObjectId" => ("o.owner_object_id", &["string", "null"]),
        "name" => ("o.name", &["string"]),
        "path" => ("o.path", &["string"]),
        "position" => ("o.position", &["integer"]),
        "icon" => ("o.icon", &["string", "null"]),
        "contractKey" => ("o.contract_key", &["string"]),
        "revision" => ("r.revision", &["integer"]),
        "contractVersion" => ("r.contract_version", &["string"]),
        "archivedAt" => ("o.archived_at", &["string", "null"]),
        "effectivelyArchived" => ("o.effective_archive", &["boolean"]),
        "createdAt" => ("o.created_at", &["string"]),
        "updatedAt" => ("o.updated_at", &["string"]),
        "hasContractChildren" => (
            "(EXISTS(SELECT 1 FROM objects c WHERE c.parent_id=o.id AND c.contract_key=o.contract_key AND c.effective_archive=0))",
            &["boolean"],
        ),
        _ => return None,
    })
}
struct Expression {
    value: String,
    kind: String,
    is_null: String,
}
struct Builder {
    bindings: Vec<Value>,
    definitions: Vec<IvyJson>,
    predicates: usize,
}
impl Builder {
    fn bind(&mut self, value: Value) -> String {
        let name = format!("$p{}", self.bindings.len());
        self.bindings.push(value);
        name
    }
    fn field(&mut self, name: &JsString, operands: &[&IvyJson]) -> IvyResult<Expression> {
        let mut definitions = vec![];
        let expression = if name
            .0
            .starts_with(&"object.".encode_utf16().collect::<Vec<_>>())
        {
            let (column, types) = metadata(&text(&JsString(name.0[7..].to_vec())))
                .ok_or_else(|| IvyError::new("invalid_arguments", "Unknown Object query field."))?;
            let rank = if types.contains(&"boolean") {
                1
            } else if types.contains(&"integer") {
                2
            } else {
                3
            };
            definitions.push(types.iter().map(|v| v.to_string()).collect::<Vec<_>>());
            Expression {
                value: column.into(),
                kind: format!("CASE WHEN {column} IS NULL THEN 9 ELSE {rank} END"),
                is_null: format!("{column} IS NULL"),
            }
        } else {
            require(
                name.0
                    .starts_with(&"data:/".encode_utf16().collect::<Vec<_>>()),
                "invalid_arguments",
                "Query data fields require a JSON pointer.",
            )?;
            let pointer = JsString(name.0[5..].to_vec());
            pointer_parts(&pointer)?;
            let parameter = self.bind(Value::Text(text(&pointer)));
            let json = "CASE WHEN r.encoding='json' THEN r.content ELSE NULL END";
            for definition in &self.definitions {
                definitions.push(match definition.get("jsonSchema") {
                    Some(s) => scalar_types(s, &pointer)?,
                    None => vec![],
                });
            }
            Expression {
                value: format!("ivy_value({json},{parameter})"),
                kind: format!("ivy_type({json},{parameter})"),
                is_null: format!("ivy_is_null({json},{parameter})=1"),
            }
        };
        for types in definitions {
            require(
                !types.is_empty(),
                "invalid_arguments",
                "Field is not an admitted scalar in every selected contract version.",
            )?;
            for operand in operands {
                let kind = match operand {
                    IvyJson::Null => "null",
                    IvyJson::Bool(_) => "boolean",
                    IvyJson::Number(_) => "number",
                    IvyJson::String(_) => "string",
                    _ => return Err(IvyError::internal()),
                };
                let integer = operand
                    .as_f64()
                    .is_some_and(|v| v.is_finite() && v.fract() == 0.0);
                require(
                    types
                        .iter()
                        .any(|t| t == kind || (kind == "number" && integer && t == "integer")),
                    "invalid_arguments",
                    "Predicate operand is incompatible with the selected contract versions.",
                )?;
            }
        }
        Ok(expression)
    }
    fn predicate(&mut self, predicate: &IvyJson, depth: usize) -> IvyResult<String> {
        self.predicates += 1;
        require(
            self.predicates <= 64 && depth <= 8,
            "limit_exceeded",
            "Query predicate exceeds its complexity limit.",
        )?;
        let op = argument(predicate.get("op").and_then(IvyJson::as_js_string))?.to_utf8()?;
        if op == "and" || op == "or" {
            let args = argument(predicate.get("args").and_then(IvyJson::as_array))?;
            let parts = args
                .iter()
                .map(|a| self.predicate(a, depth + 1))
                .collect::<IvyResult<Vec<_>>>()?;
            return Ok(format!(
                "({})",
                parts.join(if op == "and" { " AND " } else { " OR " })
            ));
        }
        if op == "not" {
            return Ok(format!(
                "(NOT {})",
                self.predicate(argument(predicate.get("arg"))?, depth + 1)?
            ));
        }
        let name = predicate
            .get("field")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(|| IvyError::new("invalid_arguments", "Invalid query predicate."))?;
        let values = if op == "isNull" {
            vec![]
        } else if op == "in" {
            argument(predicate.get("value").and_then(IvyJson::as_array))?
                .iter()
                .collect()
        } else {
            vec![argument(predicate.get("value"))?]
        };
        let field = self.field(name, &values)?;
        if op == "isNull" {
            return Ok(format!("({})", field.is_null));
        }
        if op == "contains" {
            let value = self.bind(sql_value(values[0])?);
            return Ok(format!(
                "({}=3 AND COALESCE(instr({},{value})>0,0))",
                field.kind, field.value
            ));
        }
        let operator = match op.as_str() {
            "eq" | "in" => "=",
            "ne" => "!=",
            "gt" => ">",
            "gte" => ">=",
            "lt" => "<",
            "lte" => "<=",
            _ => return Err(IvyError::internal()),
        };
        let mut parts = vec![];
        for value in values {
            if value.is_null() {
                parts.push("0".into());
                continue;
            }
            let kind = match value {
                IvyJson::Bool(_) => 1,
                IvyJson::Number(_) => 2,
                _ => 3,
            };
            let bound = self.bind(sql_value(value)?);
            parts.push(format!(
                "({}={kind} AND COALESCE({} COLLATE BINARY {operator} {bound},0))",
                field.kind, field.value
            ));
        }
        Ok(format!("({})", parts.join(" OR ")))
    }
}

/// The caller admits ObjectQuery. Query projection does not admit object writes.
pub fn query_objects(store: &Store, params: &IvyJson) -> IvyResult<IvyJson> {
    let key = argument(params.get("contractKey").and_then(IvyJson::as_js_string))?;
    let media: Option<String> = store
        .connection()
        .query_row(
            "SELECT media_type FROM contract_families WHERE key=?",
            [text(key)],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| store.query_error(e))?;
    let media = media
        .ok_or_else(|| IvyError::new("contract_not_found", "Data Contract family not found."))?;
    let include_content = params
        .get("includeContent")
        .and_then(IvyJson::as_bool)
        .unwrap_or(false);
    let json_media = media.strip_prefix("application/").is_some_and(|sub| {
        sub == "json"
            || sub.strip_suffix("+json").is_some_and(|prefix| {
                !prefix.is_empty()
                    && prefix
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
            })
    });
    require(
        !include_content || json_media,
        "invalid_arguments",
        "Query content is available only for JSON contracts.",
    )?;
    let versions = params.get("contractVersions").and_then(IvyJson::as_array);
    let mut builder = Builder {
        bindings: vec![],
        definitions: vec![],
        predicates: 0,
    };
    if let Some(versions) = versions {
        for version in versions {
            builder
                .definitions
                .push(store.contract(key, argument(version.as_js_string())?)?);
        }
    }
    let defaults = vec![IvyJson::from("object.name"), IvyJson::from("object.path")];
    let projection = params
        .get("select")
        .and_then(IvyJson::as_array)
        .unwrap_or(&defaults);
    let ordering = params
        .get("orderBy")
        .and_then(IvyJson::as_array)
        .unwrap_or(&[]);
    let mut identity = IvyJson::object([("method", IvyJson::from("objects.query"))]);
    for (k, v) in argument(params.as_object())? {
        if k != &JsString::from("cursor") {
            identity.insert(k.clone(), v.clone());
        }
    }
    let boundary = params
        .get("cursor")
        .and_then(IvyJson::as_js_string)
        .filter(|v| !v.0.is_empty())
        .map(|c| store.read_cursor(&identity, c))
        .transpose()?
        .flatten();
    let boundary = boundary.as_ref();
    let mut columns = vec![
        "o.id".into(),
        "r.revision".into(),
        "r.contract_version".into(),
    ];
    for (prefix, fields) in [("s", projection), ("k", ordering)] {
        for (index, field) in fields.iter().enumerate() {
            let name = if prefix == "s" {
                field
            } else {
                argument(field.get("field"))?
            };
            let expr = builder.field(argument(name.as_js_string())?, &[])?;
            columns.push(format!("{} AS {prefix}{index}t", expr.kind));
            columns.push(format!("{} AS {prefix}{index}v", expr.value));
        }
    }
    let mut conditions = vec![
        format!("o.contract_key={}", builder.bind(Value::Text(text(key)))),
        "ivy_budget()".into(),
    ];
    if params.get("includeArchived").and_then(IvyJson::as_bool) != Some(true) {
        conditions.push("o.effective_archive=0".into());
    }
    if let Some(versions) = versions {
        let names = versions
            .iter()
            .map(|v| Ok(builder.bind(sql_value(v)?)))
            .collect::<IvyResult<Vec<_>>>()?;
        conditions.push(format!("r.contract_version IN ({})", names.join(",")));
    }
    if let Some(predicate) = params.get("where") {
        conditions.push(builder.predicate(predicate, 0)?);
    }
    if let Some(boundary) = boundary {
        require(
            boundary
                .get("sort")
                .and_then(IvyJson::as_array)
                .is_some_and(|s| s.len() == ordering.len())
                && boundary.get("id").and_then(IvyJson::as_js_string).is_some(),
            "invalid_cursor",
            "Invalid query boundary.",
        )?;
    }
    let mut sorting = vec![];
    let mut continuation = vec![];
    let mut prefix = vec![];
    for (index, order) in ordering.iter().enumerate() {
        let desc = order.get("direction") == Some(&IvyJson::from("desc"));
        sorting.extend([
            format!("k{index}t ASC"),
            format!(
                "k{index}v COLLATE BINARY {}",
                if desc { "DESC" } else { "ASC" }
            ),
        ]);
        if let Some(boundary) = boundary {
            let entry = &boundary.get("sort").unwrap().as_array().unwrap()[index];
            let kind = entry.get("type").ok_or_else(IvyError::internal)?;
            let value = entry.get("value").ok_or_else(IvyError::internal)?;
            let t = builder.bind(sql_value(kind)?);
            let v = builder.bind(sql_value(value)?);
            let term = |prefix: &[String], expression: String| {
                format!(
                    "({})",
                    prefix
                        .iter()
                        .cloned()
                        .chain([expression])
                        .collect::<Vec<_>>()
                        .join(" AND ")
                )
            };
            continuation.push(term(&prefix, format!("k{index}t > {t}")));
            prefix.push(format!("k{index}t = {t}"));
            if kind != &IvyJson::Number(9.0) {
                continuation.push(term(
                    &prefix,
                    format!(
                        "k{index}v COLLATE BINARY {} {v}",
                        if desc { "<" } else { ">" }
                    ),
                ));
            }
            prefix.push(format!("k{index}v IS {v}"));
        }
    }
    sorting.push("id ASC".into());
    if let Some(boundary) = boundary {
        prefix.push(format!(
            "id > {}",
            builder.bind(sql_value(boundary.get("id").unwrap())?)
        ));
        continuation.push(format!("({})", prefix.join(" AND ")));
    }
    let limit = params
        .get("limit")
        .and_then(IvyJson::as_f64)
        .unwrap_or(50.0) as usize;
    let count = builder.bind(Value::Integer((limit + 1) as i64));
    let sql = format!(
        "WITH matched AS (SELECT {} FROM objects o JOIN revisions r ON r.object_id=o.id AND r.revision=o.current_revision WHERE {}) SELECT * FROM matched {} ORDER BY {} LIMIT {count}",
        columns.join(","),
        conditions.join(" AND "),
        if boundary.is_some() {
            format!("WHERE {}", continuation.join(" OR "))
        } else {
            String::new()
        },
        sorting.join(",")
    );
    let mut statement = store
        .connection()
        .prepare_cached(&sql)
        .map_err(|e| store.query_error(e))?;
    for (index, value) in builder.bindings.iter().enumerate() {
        if let Some(position) = statement
            .parameter_index(&format!("$p{index}"))
            .map_err(|e| store.query_error(e))?
        {
            statement
                .raw_bind_parameter(position, value)
                .map_err(|e| store.query_error(e))?;
        }
    }
    let mut query = statement.raw_query();
    let mut rows = vec![];
    while let Some(row) = query.next().map_err(|e| store.query_error(e))? {
        rows.push(
            (0..columns.len())
                .map(|i| row_value(row, i))
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| store.query_error(e))?,
        );
    }
    let mut items = vec![];
    let mut bytes = 128;
    for row in rows.iter().take(limit) {
        let mut values = IvyJson::object([] as [(&str, IvyJson); 0]);
        for (index, name) in projection.iter().enumerate() {
            let value = if row[3 + 2 * index] == IvyJson::Number(1.0) {
                IvyJson::Bool(row[4 + 2 * index].as_f64().is_some_and(|v| v != 0.0))
            } else {
                row[4 + 2 * index].clone()
            };
            values.insert(argument(name.as_js_string())?.clone(), value);
        }
        let mut item = IvyJson::object([
            ("objectId", row[0].clone()),
            ("revision", row[1].clone()),
            ("contractVersion", row[2].clone()),
            ("values", values),
        ]);
        if include_content {
            item.insert(
                "document",
                crate::objects::read(
                    store,
                    crate::objects::Location::Id(row[0].as_js_string().unwrap()),
                    Some(row[1].as_f64().unwrap() as u64),
                )?,
            );
        }
        let size = canonical(&item, MANAGEMENT_FRAME_BYTES)?.len();
        if bytes + size > PAGE_BYTES {
            require(
                !items.is_empty(),
                "result_too_large",
                "A selected query row exceeds the response limit.",
            )?;
            break;
        }
        bytes += size;
        items.push(item);
    }
    let cursor = if !items.is_empty() && rows.len() > items.len() {
        let row = &rows[items.len() - 1];
        let start = 3 + projection.len() * 2;
        let sort = ordering
            .iter()
            .enumerate()
            .map(|(i, _)| {
                IvyJson::object([
                    ("type", row[start + 2 * i].clone()),
                    ("value", row[start + 2 * i + 1].clone()),
                ])
            })
            .collect();
        Some(store.cursor(
            &identity,
            &IvyJson::object([("id", row[0].clone()), ("sort", IvyJson::Array(sort))]),
        )?)
    } else {
        None
    };
    Ok(page(items, cursor))
}
