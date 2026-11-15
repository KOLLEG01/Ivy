//! Declarative SVG images. Templates see data, output options and render time;
//! they cannot execute JavaScript, read files or request external resources.
use crate::{png, validation::OutputOptions};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use handlebars::{Handlebars, handlebars_helper};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, json};
use resvg::{tiny_skia, usvg};
use serde_json::Value;
use std::{
    io::Write,
    sync::{Arc, LazyLock},
};

const MAX_SVG_BYTES: usize = 1024 * 1024;
static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    Arc::new(fonts)
});
static SESSION: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().to_string());

fn error(detail: impl std::fmt::Display) -> IvyError {
    IvyError::new(
        "dashboard_render_failed",
        format!("SVG image rendering failed: {detail}"),
    )
}
fn date(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|d| d.with_timezone(&Utc))
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|d| d.and_utc())
        })
}
fn number(value: &Value, digits: u64, locale: &str) -> String {
    let Some(value) = value.as_f64().filter(|n| n.is_finite()) else {
        return "—".into();
    };
    let text = format!("{:.*}", digits.min(6) as usize, value);
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let (group, decimal) = if locale.starts_with("de") {
        ('.', ',')
    } else {
        (',', '.')
    };
    let mut formatted = String::new();
    let (sign, whole) = whole.strip_prefix('-').map_or(("", whole), |v| ("-", v));
    formatted.push_str(sign);
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            formatted.push(group);
        }
        formatted.push(c);
    }
    if !fraction.is_empty() {
        formatted.push(decimal);
        formatted.push_str(fraction);
    }
    formatted
}
static TEMPLATES: LazyLock<Handlebars<'static>> = LazyLock::new(|| {
    let mut templates = Handlebars::new();
    templates.set_strict_mode(false);
    handlebars_helper!(num: |v: Value, digits:u64, locale:str| number(&v,digits,locale));
    handlebars_helper!(finite: |v: Value| v.as_f64().is_some_and(f64::is_finite));
    handlebars_helper!(coalesce: |a: Value, b: Value| if a.is_null() || a.as_str()==Some("") { b } else { a });
    handlebars_helper!(integer: |v: Value| v.as_f64().is_some_and(|n| n.is_finite() && n.fract()==0.0 && n.abs()<=9_007_199_254_740_991.0));
    handlebars_helper!(same_month: |v: Value, now:str| v.as_str().and_then(date).zip(date(now)).is_some_and(|(a,b)| a.year()==b.year() && a.month()==b.month()));
    handlebars_helper!(stale: |v: Value, now:str, seconds:i64| v.as_str().and_then(date).zip(date(now)).is_none_or(|(a,b)| (b-a).num_milliseconds()>seconds.saturating_mul(1000)));
    handlebars_helper!(datetime: |v: Value, format:str, zone:str, locale:str| {
        v.as_str().and_then(date).and_then(|d| {
            let zone: chrono_tz::Tz = zone.parse().ok()?;
            let local = d.with_timezone(&zone);
            let format = if locale.starts_with("de") {
                let months = ["Januar","Februar","März","April","Mai","Juni","Juli","August","September","Oktober","November","Dezember"];
                format.replace("%B", months[local.month0() as usize])
            } else { format.into() };
            Some(local.format(&format).to_string())
        }).unwrap_or_else(|| "—".into())
    });
    for (name, helper) in [
        (
            "number",
            Box::new(num) as Box<dyn handlebars::HelperDef + Send + Sync>,
        ),
        ("finite", Box::new(finite)),
        ("integer", Box::new(integer)),
        ("coalesce", Box::new(coalesce)),
        ("sameMonth", Box::new(same_month)),
        ("stale", Box::new(stale)),
        ("date", Box::new(datetime)),
    ] {
        templates.register_helper(name, helper);
    }
    templates
});

struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + bytes.len() > MAX_SVG_BYTES {
            return Err(std::io::Error::other("SVG exceeds 1 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct Prepared {
    svg: String,
    pub etag: String,
    output: OutputOptions,
}
pub fn prepare(
    template: &str,
    data: J,
    output: OutputOptions,
    now: DateTime<Utc>,
) -> IvyResult<Prepared> {
    let context = J::object([
        ("data", data),
        ("now", now.to_rfc3339().into()),
        (
            "output",
            J::object([
                ("width", J::Number(output.width.into())),
                ("height", J::Number(output.height.into())),
                ("colorMode", output.color_mode().into()),
            ]),
        ),
    ]);
    let context: Value = serde_json::from_str(&json::stringify(
        &context,
        ivy_contracts::limits::MANAGEMENT_FRAME_BYTES,
    )?)
    .map_err(error)?;
    let mut svg = Limited(Vec::new());
    TEMPLATES
        .render_template_to_write(template, &context, &mut svg)
        .map_err(error)?;
    let svg = String::from_utf8(svg.0).map_err(error)?;
    let identity = format!(
        "{}:{}:{}:{}:{svg}",
        *SESSION,
        output.width,
        output.height,
        output.color_mode()
    );
    let etag = format!(
        "W/\"{}\"",
        &ivy_contracts::hash::digest(identity.as_bytes())[7..]
    );
    Ok(Prepared { svg, etag, output })
}
pub fn matches(condition: &str, etag: &str) -> bool {
    condition.split(',').any(|value| {
        let value = value.trim();
        value == "*"
            || value.strip_prefix("W/").unwrap_or(value) == etag.strip_prefix("W/").unwrap_or(etag)
    })
}
impl Prepared {
    pub fn render(self) -> IvyResult<Vec<u8>> {
        let options = usvg::Options {
            fontdb: FONTS.clone(),
            image_href_resolver: usvg::ImageHrefResolver {
                resolve_string: Box::new(|_, _| None),
                ..Default::default()
            },
            ..Default::default()
        };
        let tree = usvg::Tree::from_str(&self.svg, &options).map_err(error)?;
        // Fit marked text using the renderer's actual shaped glyph bounds.
        // Font sizes shrink only when necessary; ordinary SVG remains unchanged.
        let xml = usvg::roxmltree::Document::parse(&self.svg).map_err(error)?;
        let mut replacements = Vec::new();
        for node in xml.descendants().filter(|n| n.has_tag_name("text")) {
            let Some((id, width, font)) = node
                .attribute("id")
                .zip(node.attribute("data-fit-width"))
                .zip(node.attribute_node("font-size"))
                .map(|((id, width), font)| (id, width, font))
            else {
                continue;
            };
            let width: f32 = width.parse().map_err(error)?;
            let size: f32 = font.value().parse().map_err(error)?;
            if !width.is_finite() || width <= 0.0 || !size.is_finite() || size <= 0.0 {
                return Err(error("Invalid text fitting bounds"));
            }
            if let Some(text) = tree.node_by_id(id) {
                let actual = text.bounding_box().map_or(0.0, |b| b.width());
                if actual > width {
                    replacements.push((font.range_value(), format!("{}", size * width / actual)));
                }
            }
        }
        let mut svg = self.svg;
        replacements.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, value) in &replacements {
            svg.replace_range(range.clone(), value);
        }
        let tree = if replacements.is_empty() {
            tree
        } else {
            usvg::Tree::from_str(&svg, &options).map_err(error)?
        };
        let mut pixels = tiny_skia::Pixmap::new(self.output.width, self.output.height)
            .ok_or_else(|| error("Invalid dimensions"))?;
        pixels.fill(tiny_skia::Color::WHITE);
        let transform = tiny_skia::Transform::from_scale(
            self.output.width as f32 / tree.size().width(),
            self.output.height as f32 / tree.size().height(),
        );
        resvg::render(&tree, transform, &mut pixels.as_mut());
        let png = pixels.encode_png().map_err(error)?;
        png::convert(png, self.output.mode).map_err(error)
    }
}
