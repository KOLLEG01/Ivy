//! Inert document projection. Run only inside the owned disposable worker.
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    io::{Cursor, Read},
};

pub const SOURCE_LIMIT: usize = 32 * 1024 * 1024;
const EXPANDED_LIMIT: u64 = 16 * 1024 * 1024;
const TEXT_LIMIT: usize = 6 * 1024 * 1024;
const NODE_LIMIT: usize = 250_000;
pub type Result<T> = std::result::Result<T, &'static str>;
const INVALID: &str = "media_document_invalid";
const LARGE: &str = "media_document_too_large";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Docx,
    Html,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub label: String,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentText {
    pub schema_version: u8,
    pub format: Format,
    pub representation: String,
    pub sections: Vec<Section>,
    pub limitations: Vec<String>,
}
impl DocumentText {
    fn new(format: Format, sections: Vec<Section>) -> Self {
        Self {
            schema_version: 1,
            format,
            representation: "document_text".into(),
            sections,
            limitations: [
                "visual_layout_not_interpreted",
                "embedded_media_not_interpreted",
                "external_resources_not_loaded",
            ]
            .map(str::to_owned)
            .into(),
        }
    }
}

// ECMAScript trim differs from Rust's Unicode White_Space (notably BOM and NEL).
fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'))
}
fn utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| INVALID)
}
fn xml(bytes: &[u8]) -> Result<roxmltree::Document<'_>> {
    let text = utf8(bytes)?;
    // Also reject declarations hidden in comments, matching the original gate.
    let upper = text.to_ascii_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err(INVALID);
    }
    roxmltree::Document::parse(text).map_err(|_| INVALID)
}
fn word_namespace(ns: Option<&str>) -> bool {
    matches!(
        ns,
        Some(
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                | "http://purl.oclc.org/ooxml/wordprocessingml/main"
        )
    )
}
fn word_text(bytes: &[u8]) -> Result<String> {
    let doc = xml(bytes)?;
    enum Pending<'a, 'b> {
        Node(roxmltree::Node<'a, 'b>),
        Text(&'static str),
    }
    let mut pending = vec![Pending::Node(doc.root())];
    let mut text = String::new();
    let mut count = 0;
    while let Some(part) = pending.pop() {
        let node = match part {
            Pending::Text(s) => {
                text.push_str(s);
                continue;
            }
            Pending::Node(n) => n,
        };
        count += 1;
        if count > NODE_LIMIT {
            return Err(LARGE);
        }
        if node.is_element() && word_namespace(node.tag_name().namespace()) {
            match node.tag_name().name() {
                "t" => {
                    for n in node.descendants().filter(|n| n.is_text()) {
                        text.push_str(n.text().unwrap_or(""));
                    }
                    continue;
                }
                "tab" => text.push('\t'),
                "br" | "cr" => text.push('\n'),
                "p" | "tr" => pending.push(Pending::Text("\n")),
                "tc" => pending.push(Pending::Text("\t")),
                "del" | "instrText" => continue,
                _ => (),
            }
        }
        pending.extend(node.children().rev().map(Pending::Node));
    }
    Ok(trim(&text).into())
}
fn selected_part(name: &str) -> bool {
    if matches!(
        name,
        "[Content_Types].xml"
            | "word/document.xml"
            | "word/footnotes.xml"
            | "word/endnotes.xml"
            | "word/comments.xml"
    ) {
        return true;
    }
    ["word/header", "word/footer"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(".xml"))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    })
}
fn docx(bytes: &[u8]) -> Result<DocumentText> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| INVALID)?;
    if archive.is_empty() || archive.len() > 4096 {
        return Err(LARGE);
    }
    // ZipArchive indexes by raw name and replaces duplicates. Count original
    // central records before using that index, so a hidden entry cannot bypass
    // duplicate/entry/expansion admission from the original streaming reader.
    let mut offset = usize::try_from(archive.central_directory_start()).map_err(|_| INVALID)?;
    let mut entries = 0;
    while bytes.get(offset..offset + 4) == Some(b"PK\x01\x02") {
        let header = bytes.get(offset..offset + 46).ok_or(INVALID)?;
        let length = |i| u16::from_le_bytes([header[i], header[i + 1]]) as usize;
        offset = offset
            .checked_add(46 + length(28) + length(30) + length(32))
            .ok_or(INVALID)?;
        if offset > bytes.len() {
            return Err(INVALID);
        }
        entries += 1;
        if entries > 4096 {
            return Err(LARGE);
        }
    }
    if entries != archive.len() {
        return Err(INVALID);
    }
    let mut names = HashSet::new();
    let mut retained = BTreeMap::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| INVALID)?;
        let name = entry.name().to_owned();
        if name.contains('\\')
            || name.starts_with('/')
            || name.split('/').any(|p| p == "..")
            || name.as_bytes().get(1) == Some(&b':')
            || !names.insert(name.clone())
            || entry.encrypted()
        {
            return Err(INVALID);
        }
        total = total.checked_add(entry.size()).ok_or(LARGE)?;
        if total > EXPANDED_LIMIT {
            return Err(LARGE);
        }
        if !selected_part(&name) {
            continue;
        }
        if retained.len() >= 129 {
            return Err(LARGE);
        }
        let size = entry.size();
        let mut data = Vec::new();
        (&mut entry)
            .take(size + 1)
            .read_to_end(&mut data)
            .map_err(|_| INVALID)?;
        // zip verifies CRC while reading; a short read must not become partial text.
        if data.len() as u64 != size {
            return Err(INVALID);
        }
        retained.insert(name, data);
    }
    let types = xml(retained.get("[Content_Types].xml").ok_or(INVALID)?)?;
    let mut main = false;
    for n in types.descendants().filter(|n| {
        n.has_tag_name((
            "http://schemas.openxmlformats.org/package/2006/content-types",
            "Override",
        ))
    }) {
        if n.attribute("PartName") == Some("/word/document.xml") {
            main = n.attribute("ContentType")
                == Some(
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
                );
        }
    }
    if !main || !retained.contains_key("word/document.xml") {
        return Err(INVALID);
    }
    let mut sections = vec![Section {
        label: "word/document.xml".into(),
        text: word_text(&retained["word/document.xml"])?,
    }];
    for (label, value) in &retained {
        if label != "word/document.xml" && label != "[Content_Types].xml" {
            sections.push(Section {
                label: label.clone(),
                text: word_text(value)?,
            });
        }
    }
    Ok(DocumentText::new(Format::Docx, sections))
}

fn content_charset(value: &str) -> Option<&str> {
    let lower = value.to_ascii_lowercase();
    let start = lower.find("charset")? + 7;
    let rest = value[start..]
        .trim_start_matches(char::is_whitespace)
        .strip_prefix('=')?
        .trim_start_matches(char::is_whitespace);
    let end = rest
        .find(|c: char| c == ';' || c.is_whitespace())
        .unwrap_or(rest.len());
    (end > 0).then_some(&rest[..end])
}
fn html(bytes: &[u8]) -> Result<DocumentText> {
    let dom = parse_document(RcDom::default(), Default::default()).one(utf8(bytes)?);
    enum Pending {
        Node(Handle),
        Text(String),
    }
    let mut pending = vec![Pending::Node(dom.document.clone())];
    let mut text = String::new();
    let mut count = 0;
    while let Some(part) = pending.pop() {
        let node = match part {
            Pending::Text(s) => {
                text.push_str(&s);
                continue;
            }
            Pending::Node(n) => n,
        };
        count += 1;
        if count > NODE_LIMIT {
            return Err(LARGE);
        }
        match &node.data {
            NodeData::Text { contents } => {
                text.push_str(&contents.borrow());
                continue;
            }
            NodeData::Element { name, attrs, .. } => {
                let tag = name.local.as_ref();
                let attrs = attrs.borrow();
                let attr = |key: &str| {
                    attrs
                        .iter()
                        .find(|a| a.name.local.as_ref() == key)
                        .map(|a| a.value.as_ref())
                };
                if tag == "meta" {
                    let charset =
                        attr("charset").or_else(|| attr("content").and_then(content_charset));
                    if charset.is_some_and(|s| {
                        !s.is_empty()
                            && !s.eq_ignore_ascii_case("utf-8")
                            && !s.eq_ignore_ascii_case("utf8")
                    }) {
                        return Err("media_text_encoding_unsupported");
                    }
                }
                if matches!(
                    tag,
                    "script" | "style" | "template" | "iframe" | "object" | "embed"
                ) || attr("hidden").is_some()
                    || attr("aria-hidden") == Some("true")
                {
                    continue;
                }
                let block = matches!(
                    tag,
                    "address"
                        | "article"
                        | "aside"
                        | "blockquote"
                        | "div"
                        | "dl"
                        | "dt"
                        | "dd"
                        | "footer"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "header"
                        | "hr"
                        | "li"
                        | "main"
                        | "ol"
                        | "p"
                        | "pre"
                        | "section"
                        | "table"
                        | "tr"
                        | "ul"
                );
                if tag == "br" || block {
                    text.push('\n');
                }
                if block {
                    pending.push(Pending::Text("\n".into()));
                }
                if matches!(tag, "td" | "th") {
                    pending.push(Pending::Text("\t".into()));
                }
                if tag == "img" {
                    if let Some(alt) = attr("alt").filter(|s| !s.is_empty()) {
                        text.push_str(&format!(" [{alt}] "));
                    }
                }
                if tag == "a" {
                    if let Some(href) = attr("href").filter(|s| {
                        ["http:", "https:", "mailto:"]
                            .iter()
                            .any(|p| s.to_ascii_lowercase().starts_with(p))
                    }) {
                        pending.push(Pending::Text(format!(" ({href})")));
                    }
                }
            }
            _ => (),
        }
        pending.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .cloned()
                .map(Pending::Node),
        );
    }
    Ok(DocumentText::new(
        Format::Html,
        vec![Section {
            label: "HTML document".into(),
            text: trim(&text).into(),
        }],
    ))
}

/// PDF candidate for differential qualification; not yet a replacement for PDF.js.
fn pdf(bytes: &[u8]) -> Result<DocumentText> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("media_signature_mismatch");
    }
    let doc = pdf_extract::Document::load_mem(bytes).map_err(|_| INVALID)?;
    let pages = doc.get_pages();
    if pages.is_empty() || pages.len() > 128 {
        return Err(LARGE);
    }
    let mut sections = Vec::new();
    let mut total = 0;
    for page in pages.keys() {
        let mut text = String::new();
        pdf_extract::output_doc_page(
            &doc,
            &mut pdf_extract::PlainTextOutput::new(&mut text),
            *page,
        )
        .map_err(|_| INVALID)?;
        total += text.len();
        if total > TEXT_LIMIT {
            return Err(LARGE);
        }
        sections.push(Section {
            label: format!("Page {page}"),
            text: trim(&text).into(),
        });
    }
    let mut result = DocumentText::new(Format::Pdf, sections);
    result
        .limitations
        .push("pdf_annotations_and_forms_not_interpreted".into());
    if result.sections.iter().any(|s| s.text.is_empty()) {
        result.limitations.push("empty_pdf_pages".into());
    }
    Ok(result)
}

pub fn extract(format: Format, bytes: &[u8]) -> Result<DocumentText> {
    if bytes.len() > SOURCE_LIMIT {
        return Err(LARGE);
    }
    let value = match format {
        Format::Pdf => pdf(bytes),
        Format::Docx => docx(bytes),
        Format::Html => html(bytes),
    }?;
    if value.sections.is_empty()
        || value.sections.len() > 128
        || value.sections.iter().any(|s| {
            s.label.is_empty()
                || s.label.chars().count() > 256
                || s.text.chars().count() > TEXT_LIMIT
        })
    {
        return Err(INVALID);
    }
    if !value.sections.iter().any(|s| !trim(&s.text).is_empty()) {
        return Err("media_document_no_text");
    }
    Ok(value)
}
