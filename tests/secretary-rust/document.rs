use ivy_secretary::document::{Format, extract};
use std::io::{Cursor, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

fn docx(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("[Content_Types].xml", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    for (name, bytes) in parts {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn word(text: &str) -> String {
    format!(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
    )
}

#[test]
fn html_is_inert_and_keeps_projection_order() {
    let text = "<!doctype html><meta charset=UTF-8><h1>Original 🌍</h1><p>Agenda &amp; context</p><script>fetch('never')</script><img src='https://never.invalid' alt='Chart alternative'><a href='https://example.test/context'>Source</a><p hidden>Hidden</p>";
    let result = extract(Format::Html, text.as_bytes()).unwrap();
    assert_eq!(
        result.sections[0].text,
        "Original 🌍\n\nAgenda & context\n [Chart alternative] Source (https://example.test/context)"
    );
    assert_eq!(
        extract(Format::Html, b"<meta charset=windows-1252><p>text</p>"),
        Err("media_text_encoding_unsupported")
    );
    assert_eq!(
        extract(Format::Html, b"<script>not text</script>"),
        Err("media_document_no_text")
    );
    assert!(extract(Format::Html, &[0xff]).is_err());
}

#[test]
fn docx_preserves_namespaces_order_and_excludes_deleted_text() {
    let main = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Visible ä</w:t></w:r><w:del><w:r><w:t>Deleted secret</w:t></w:r></w:del></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Cell A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Cell B</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#;
    let header = word("Header");
    let comment = word("Comment");
    let bytes = docx(&[
        ("word/header1.xml", header.as_bytes()),
        ("word/document.xml", main.as_bytes()),
        ("word/comments.xml", comment.as_bytes()),
        (
            "word/_rels/document.xml.rels",
            b"ignored external relationships",
        ),
    ]);
    let result = extract(Format::Docx, &bytes).unwrap();
    assert_eq!(
        result
            .sections
            .iter()
            .map(|s| s.label.as_str())
            .collect::<Vec<_>>(),
        ["word/document.xml", "word/comments.xml", "word/header1.xml"]
    );
    assert_eq!(result.sections[0].text, "Visible ä\nCell A\n\tCell B");
}

#[test]
fn docx_rejects_entities_malformed_xml_expansion_and_crc() {
    let long_header = format!("word/header{}.xml", "1".repeat(256));
    assert_eq!(
        extract(
            Format::Docx,
            &docx(&[
                ("word/document.xml", word("Hello").as_bytes()),
                (&long_header, word("Header").as_bytes()),
            ])
        ),
        Err("media_document_invalid")
    );
    for xml in [
        "<!DOCTYPE document [<!ENTITY x SYSTEM 'file:///never'>]><document>&x;</document>",
        "<w:document>unclosed",
    ] {
        assert_eq!(
            extract(
                Format::Docx,
                &docx(&[("word/document.xml", xml.as_bytes())])
            ),
            Err("media_document_invalid")
        );
    }
    let large = word(&"x".repeat(16 * 1024 * 1024));
    assert_eq!(
        extract(
            Format::Docx,
            &docx(&[("word/document.xml", large.as_bytes())])
        ),
        Err("media_document_too_large")
    );
    let mut bytes = docx(&[("word/document.xml", word("Hello").as_bytes())]);
    let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    bytes[central + 16] ^= 1;
    assert_eq!(extract(Format::Docx, &bytes), Err("media_document_invalid"));
}

#[test]
fn pdf_signature_is_checked_before_parser() {
    assert_eq!(
        extract(Format::Pdf, b"not PDF"),
        Err("media_signature_mismatch")
    );
}

#[test]
fn duplicate_docx_entries_cannot_be_hidden_by_zip_indexing() {
    let original = word("Original");
    let other = word("Replacement");
    let mut bytes = docx(&[
        ("word/document.xml", original.as_bytes()),
        ("word/ducoment.xml", other.as_bytes()),
    ]);
    let positions = bytes
        .windows(17)
        .enumerate()
        .filter_map(|(i, w)| (w == b"word/ducoment.xml").then_some(i))
        .collect::<Vec<_>>();
    for i in positions {
        bytes[i..i + 17].copy_from_slice(b"word/document.xml");
    }
    assert_eq!(extract(Format::Docx, &bytes), Err("media_document_invalid"));
}
