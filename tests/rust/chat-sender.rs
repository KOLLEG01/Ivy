#[path = "../../services/chat-bridge/rust/src/whatsapp/sender.rs"]
mod sender;
use sender::{MessageKey, admitted_sender, jid_phone, needs_lid_mapping, phone};
use std::{
    fmt::Write as _,
    io::Write as _,
    process::{Command, Stdio},
};

fn oracle(rows: &[Vec<String>]) -> String {
    let mut input = String::new();
    for row in rows {
        for (index, field) in row.iter().enumerate() {
            if index > 0 {
                input.push('\t');
            }
            for byte in field.as_bytes() {
                write!(input, "{byte:02x}").unwrap();
            }
        }
        input.push('\n');
    }
    let mut child = Command::new("node")
        .current_dir(
            option_env!("CARGO_MANIFEST_DIR")
                .map(|p| std::path::Path::new(p).join("../../.."))
                .unwrap_or_else(|| std::env::current_dir().unwrap()),
        )
        .arg("tests/rust/fixtures/chat-sender-oracle.mjs")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let written = child.stdin.take().unwrap().write_all(input.as_bytes());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    written.unwrap();
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn exact_phone_and_device_jid_formats_match_original_source() {
    let mut values = Vec::new();
    for length in 0..=17 {
        let number = "1".repeat(length);
        for prefix in ["", "+", "00", "0", " ", "++", "+00"] {
            for suffix in [
                "",
                "\n",
                "\r",
                "\r\n",
                "\u{2028}",
                " ",
                "@s.whatsapp.net",
                "@c.us",
                ":0@s.whatsapp.net",
                ":123@c.us",
                ":@c.us",
                ":1:2@c.us",
                "@lid",
            ] {
                values.push(format!("{prefix}{number}{suffix}"));
            }
        }
    }
    values.extend(
        [
            "١٢٣٤٥٦٧",
            "１２３４５６７",
            "15555550200:１２@s.whatsapp.net",
            "15555550200:123@S.WHATSAPP.NET",
        ]
        .map(str::to_owned),
    );
    let rows: Vec<_> = values
        .iter()
        .map(|v| vec!["parse".into(), v.clone()])
        .collect();
    let expected = oracle(&rows);
    let actual = values
        .iter()
        .map(|s| {
            format!(
                "{}\t{}",
                phone(s).unwrap_or("-"),
                jid_phone(s).unwrap_or("-")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(actual, expected);
}

#[test]
fn sender_admission_and_lookup_guards_match_original_source() {
    let phones = ["+15555550200", "0015555550201"];
    let mut rows = Vec::new();
    let mut actual = Vec::new();
    for jid in [
        "15555550200@s.whatsapp.net",
        "15555550200:7@s.whatsapp.net",
        "15555550201@s.whatsapp.net",
        "15555550999@s.whatsapp.net",
        "15555550200@c.us",
        "123@g.us",
        "123@lid",
        "@lid",
        "invalid@s.whatsapp.net",
        "123@lid\n",
    ] {
        for mapped in [
            "",
            "15555550200@s.whatsapp.net",
            "15555550201:4@c.us",
            "invalid",
        ] {
            for alt in [
                "",
                "15555550200@s.whatsapp.net",
                "15555550201@s.whatsapp.net",
                "invalid",
            ] {
                for (id, from_me, participant) in [
                    ("m", false, ""),
                    ("", false, ""),
                    ("m", true, ""),
                    ("m", false, "participant"),
                ] {
                    let key = MessageKey {
                        id,
                        remote_jid: jid,
                        remote_jid_alt: alt,
                        from_me,
                        participant,
                    };
                    actual.push(format!(
                        "{}\t{}",
                        admitted_sender(&key, Some(mapped), &phones)
                            .map(|n| n.to_string())
                            .unwrap_or("-".into()),
                        usize::from(needs_lid_mapping(&key))
                    ));
                    rows.push(vec![
                        "admit".into(),
                        id.into(),
                        jid.into(),
                        alt.into(),
                        if from_me { "1" } else { "0" }.into(),
                        participant.into(),
                        mapped.into(),
                        phones[0].into(),
                        phones[1].into(),
                    ]);
                }
            }
        }
    }
    assert_eq!(actual.join("\n"), oracle(&rows));
}
