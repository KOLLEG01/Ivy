use ivy_chat_bridge::whatsapp::identity::{identity, output_parts};
use ivy_contracts::{IvyJson, JsString, json::parse};

#[test]
fn ordinary_json_identities_match_node_without_canonical_reordering() {
    for raw in [
        r#"["out","notice",3500]"#,
        r#"[{"z":1,"a":2,"2":"two","1":"one"},"\ud800",0]"#,
        r#"["sender","fixture@invalid",{"type":"Buffer","data":"AAH/"}]"#,
        r#"["😃",-0,1e-7,1e21]"#,
    ] {
        let IvyJson::Array(parts) = parse(raw, usize::MAX).unwrap() else {
            unreachable!()
        };
        let output = std::process::Command::new("node").args(["--input-type=module", "-e",
            "import {createHash} from 'node:crypto'; process.stdout.write(createHash('sha256').update(JSON.stringify(JSON.parse(process.argv[1]))).digest('hex'));", raw]).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            identity(&parts).unwrap(),
            String::from_utf8(output.stdout).unwrap()
        );
    }
}

#[test]
fn chunked_ids_and_text_match_the_original_journal_formula() {
    let key = JsString::from("reply-key");
    let mut units = vec![u16::from(b'a'); 3499];
    units.extend([0xd83d, 0xde00, 0xd800, u16::from(b'b'), 0xdc00]);
    let parts = output_parts(&key, &JsString(units.clone())).unwrap();
    let output = std::process::Command::new("node").args(["--input-type=module", "-e", r#"
        import {createHash} from 'node:crypto';
        const text='a'.repeat(3499)+'\ud83d\ude00\ud800b\udc00', chars=[...text], parts=[];
        for(let offset=0;offset<chars.length;offset+=3500) parts.push({id:createHash('sha256').update(JSON.stringify(['out','reply-key',offset])).digest('hex').slice(0,32).toUpperCase(),text:chars.slice(offset,offset+3500).join('')});
        process.stdout.write(JSON.stringify(parts));
    "#]).output().unwrap();
    assert!(output.status.success());
    let expected = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let actual = IvyJson::Array(
        parts
            .into_iter()
            .map(|part| {
                IvyJson::object([
                    ("id", IvyJson::from(part.id)),
                    ("text", IvyJson::String(part.text)),
                ])
            })
            .collect(),
    );
    assert!(
        actual == expected,
        "Output chunks differ from Node's original journal formula"
    );
}
