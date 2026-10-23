//! Typed dashboard inputs and the constraints of the public service contract.
use crate::{raster::ColorMode, sources::Source};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString, json};

const MAX_REVISION: u64 = 9_007_199_254_740_991;

pub struct Definition {
    pub title: JsString,
    pub html: JsString,
    pub sources: Vec<(JsString, Source)>,
    pub refresh_seconds: u32,
    pub metadata: J,
    pub image_token: Option<JsString>,
}

impl Definition {
    pub fn parse(value: &J) -> IvyResult<Self> {
        Self::parse_at(value, "dashboard")
    }

    fn parse_at(value: &J, path: &str) -> IvyResult<Self> {
        let object = Object::new(
            value,
            path,
            &[
                "title",
                "html",
                "sources",
                "refreshSeconds",
                "metadata",
                "imageToken",
            ],
        )?;
        let mut title = object.string("title")?;
        let whitespace =
            |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(char::is_whitespace);
        let start = title
            .0
            .iter()
            .position(|u| !whitespace(u))
            .unwrap_or(title.0.len());
        let end = title
            .0
            .iter()
            .rposition(|u| !whitespace(u))
            .map_or(start, |i| i + 1);
        title.0 = title.0[start..end].to_vec();
        text_length(&title, &object.path("title"), 1, 200)?;
        let html = object.text("html", 1, 500_000)?;

        let source_path = object.path("sources");
        let pairs = object
            .required("sources")?
            .as_object()
            .ok_or_else(|| invalid(&source_path, "expected an object"))?;
        if pairs.len() > 20 {
            return Err(invalid(&source_path, "at most 20 sources are allowed"));
        }
        let mut sources = Vec::with_capacity(pairs.len());
        for (name, value) in pairs {
            if !valid_source_name(name) {
                return Err(invalid(
                    &source_path,
                    "source names must match [A-Za-z][A-Za-z0-9_]{0,63}",
                ));
            }
            let path = format!("{source_path}.{}", String::from_utf16_lossy(&name.0));
            let source = if value.get("objectId").is_some() {
                let source = Object::new(value, &path, &["objectId"])?;
                Source::Object(source.text("objectId", 1, usize::MAX)?)
            } else {
                let source = Object::new(value, &path, &["contractKey", "limit"])?;
                Source::Contract {
                    key: source.text("contractKey", 1, usize::MAX)?,
                    limit: source.integer("limit", Some(50), 1, 100)? as u32,
                }
            };
            sources.push((name.clone(), source));
        }

        let refresh_seconds = object.integer("refreshSeconds", Some(5), 0, 86400)? as u32;
        if refresh_seconds != 0 && refresh_seconds < 5 {
            return Err(invalid(
                &object.path("refreshSeconds"),
                "must be 0 or at least 5 seconds",
            ));
        }
        let metadata = value.get("metadata").cloned().unwrap_or(J::Object(vec![]));
        if metadata.as_object().is_none() {
            return Err(invalid(&object.path("metadata"), "expected an object"));
        }
        let encoded = json::encode_uri_component(&json::stringify(&metadata, usize::MAX)?.into())?;
        if metadata.get("refreshSeconds").is_some() || encoded.len() > 8000 {
            return Err(invalid(
                &object.path("metadata"),
                "must fit in 8000 encoded bytes and must not contain refreshSeconds",
            ));
        }
        let image_token = match value.get("imageToken") {
            None | Some(J::Null) => None,
            Some(_) => {
                let token = object.text("imageToken", 32, 128)?;
                if !token
                    .0
                    .iter()
                    .all(|u| matches!(u, 65..=90 | 97..=122 | 48..=57 | 95 | 45))
                {
                    return Err(invalid(
                        &object.path("imageToken"),
                        "expected a URL-safe token",
                    ));
                }
                Some(token)
            }
        };
        Ok(Self {
            title,
            html,
            sources,
            refresh_seconds,
            metadata,
            image_token,
        })
    }

    pub fn into_json(self) -> J {
        J::object([
            ("title", J::String(self.title)),
            ("html", J::String(self.html)),
            (
                "sources",
                J::Object(
                    self.sources
                        .into_iter()
                        .map(|(name, source)| {
                            let value = match source {
                                Source::Object(id) => J::object([("objectId", J::String(id))]),
                                Source::Contract { key, limit } => J::object([
                                    ("contractKey", J::String(key)),
                                    ("limit", J::Number(f64::from(limit))),
                                ]),
                            };
                            (name, value)
                        })
                        .collect(),
                ),
            ),
            ("refreshSeconds", J::Number(f64::from(self.refresh_seconds))),
            ("metadata", self.metadata),
            (
                "imageToken",
                self.image_token.map(J::String).unwrap_or(J::Null),
            ),
        ])
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OutputOptions {
    pub width: u32,
    pub height: u32,
    pub mode: ColorMode,
}

impl OutputOptions {
    pub fn parse(value: &J) -> IvyResult<Self> {
        Self::from_object(&Object::new(
            value,
            "output",
            &["width", "height", "colorMode"],
        )?)
    }

    fn from_object(object: &Object<'_>) -> IvyResult<Self> {
        let width = object.integer("width", Some(800), 100, 4096)? as u32;
        let height = object.integer("height", Some(600), 100, 4096)? as u32;
        if u64::from(width) * u64::from(height) > 8_000_000 {
            return Err(invalid(object.path, "at most 8 million pixels are allowed"));
        }
        let mode = match object.value.get("colorMode") {
            None => ColorMode::Color,
            Some(value) => match value.as_str().ok().as_deref() {
                Some("color") => ColorMode::Color,
                Some("grayscale") => ColorMode::Grayscale,
                Some("monochrome") => ColorMode::Monochrome,
                _ => {
                    return Err(invalid(
                        &object.path("colorMode"),
                        "expected color, grayscale or monochrome",
                    ));
                }
            },
        };
        Ok(Self {
            width,
            height,
            mode,
        })
    }

    pub fn color_mode(self) -> &'static str {
        match self.mode {
            ColorMode::Color => "color",
            ColorMode::Grayscale => "grayscale",
            ColorMode::Monochrome => "monochrome",
        }
    }
}

pub enum Input {
    List {
        cursor: Option<JsString>,
        limit: u32,
    },
    Read {
        id: JsString,
    },
    Save {
        value: Definition,
        target: Option<(JsString, u64)>,
    },
    Delete {
        id: JsString,
        expected_revision: u64,
    },
    Data {
        id: JsString,
    },
    Render {
        id: JsString,
        output: OutputOptions,
    },
    Image {
        id: JsString,
        token: JsString,
        output: OutputOptions,
    },
}

impl Input {
    pub fn parse(name: &str, value: &J) -> IvyResult<Self> {
        Ok(match name {
            "list" => {
                let object = Object::new(value, name, &["cursor", "limit"])?;
                Self::List {
                    cursor: object.optional_text("cursor", 0, usize::MAX)?,
                    limit: object.integer("limit", Some(50), 1, 100)? as u32,
                }
            }
            "read" | "data" => {
                let object = Object::new(value, name, &["id"])?;
                let id = object.text("id", 1, usize::MAX)?;
                if name == "read" {
                    Self::Read { id }
                } else {
                    Self::Data { id }
                }
            }
            "save" => {
                let object = Object::new(value, name, &["id", "expectedRevision", "value"])?;
                let id = object.optional_text("id", 1, usize::MAX)?;
                let revision = value
                    .get("expectedRevision")
                    .map(|v| integer(v, &object.path("expectedRevision"), 1, MAX_REVISION))
                    .transpose()?;
                let target = match (id, revision) {
                    (None, None) => None,
                    (Some(id), Some(revision)) => Some((id, revision)),
                    _ => {
                        return Err(invalid(
                            name,
                            "id and expectedRevision must be supplied together",
                        ));
                    }
                };
                Self::Save {
                    value: Definition::parse_at(object.required("value")?, "save.value")?,
                    target,
                }
            }
            "delete" => {
                let object = Object::new(value, name, &["id", "expectedRevision"])?;
                Self::Delete {
                    id: object.text("id", 1, usize::MAX)?,
                    expected_revision: object.integer("expectedRevision", None, 1, MAX_REVISION)?,
                }
            }
            "render" | "image" => {
                let fields: &[&str] = if name == "image" {
                    &["id", "token", "width", "height", "colorMode"]
                } else {
                    &["id", "width", "height", "colorMode"]
                };
                let object = Object::new(value, name, fields)?;
                let id = object.text("id", 1, usize::MAX)?;
                let output = OutputOptions::from_object(&object)?;
                if name == "image" {
                    Self::Image {
                        id,
                        token: object.text("token", 1, 128)?,
                        output,
                    }
                } else {
                    Self::Render { id, output }
                }
            }
            _ => return Err(IvyError::internal()),
        })
    }

    pub fn is_write(&self) -> bool {
        matches!(self, Self::Save { .. } | Self::Delete { .. })
    }
}

fn invalid(path: &str, message: &str) -> IvyError {
    IvyError::new("invalid_arguments", format!("Invalid {path}: {message}."))
}

struct Object<'a> {
    value: &'a J,
    path: &'a str,
}

impl<'a> Object<'a> {
    fn new(value: &'a J, path: &'a str, fields: &[&str]) -> IvyResult<Self> {
        let pairs = value
            .as_object()
            .ok_or_else(|| invalid(path, "expected an object"))?;
        for (name, _) in pairs {
            if !fields
                .iter()
                .any(|field| name.0.iter().copied().eq(field.encode_utf16()))
            {
                return Err(invalid(
                    path,
                    &format!("unrecognized field {}", String::from_utf16_lossy(&name.0)),
                ));
            }
        }
        Ok(Self { value, path })
    }

    fn path(&self, name: &str) -> String {
        format!("{}.{name}", self.path)
    }

    fn required(&self, name: &str) -> IvyResult<&'a J> {
        self.value
            .get(name)
            .ok_or_else(|| invalid(&self.path(name), "field is required"))
    }

    fn string(&self, name: &str) -> IvyResult<JsString> {
        self.required(name)?
            .as_js_string()
            .cloned()
            .ok_or_else(|| invalid(&self.path(name), "expected a string"))
    }

    fn text(&self, name: &str, min: usize, max: usize) -> IvyResult<JsString> {
        let text = self.string(name)?;
        text_length(&text, &self.path(name), min, max)?;
        Ok(text)
    }

    fn optional_text(&self, name: &str, min: usize, max: usize) -> IvyResult<Option<JsString>> {
        self.value
            .get(name)
            .map(|_| self.text(name, min, max))
            .transpose()
    }

    fn integer(&self, name: &str, default: Option<u64>, min: u64, max: u64) -> IvyResult<u64> {
        match self.value.get(name) {
            Some(value) => integer(value, &self.path(name), min, max),
            None => default.ok_or_else(|| invalid(&self.path(name), "field is required")),
        }
    }
}

fn integer(value: &J, path: &str, min: u64, max: u64) -> IvyResult<u64> {
    match value.as_f64() {
        Some(n) if n.is_finite() && n.fract() == 0.0 && n >= min as f64 && n <= max as f64 => {
            Ok(n as u64)
        }
        _ => Err(invalid(
            path,
            &format!("expected an integer between {min} and {max}"),
        )),
    }
}

fn text_length(text: &JsString, path: &str, min: usize, max: usize) -> IvyResult<()> {
    if (min..=max).contains(&text.code_point_len()) {
        Ok(())
    } else {
        Err(invalid(
            path,
            &format!("expected {min} to {max} characters"),
        ))
    }
}

fn valid_source_name(name: &JsString) -> bool {
    (1..=64).contains(&name.0.len())
        && matches!(name.0[0], 65..=90 | 97..=122)
        && name
            .0
            .iter()
            .all(|u| matches!(u, 65..=90 | 97..=122 | 48..=57 | 95))
}
