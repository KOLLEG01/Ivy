//! Inert protocol fixture, compiled explicitly by the selected native check.
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
fn pattern(amount: usize) {
    let part: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();
    let mut out = std::io::stdout().lock();
    let mut left = amount;
    while left > 0 {
        let size = left.min(part.len());
        out.write_all(&part[..size]).unwrap();
        left -= size;
    }
}
fn report(path: &Path) {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let env: BTreeMap<_, _> = std::env::vars_os().collect();
    std::fs::write(
        path,
        format!(
            "{env:?}\nstdin={}\ncwd={}\n",
            input.len(),
            std::env::current_dir().unwrap().display()
        ),
    )
    .unwrap();
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "bytes") {
        report(Path::new(&args[2]));
        std::io::stderr()
            .write_all(&vec![b'P'; 256 * 1024])
            .unwrap();
        pattern(args[1].parse().unwrap());
        return;
    }
    if args.first().is_some_and(|s| s == "fail") {
        std::process::exit(7);
    }
    if args.first().is_some_and(|s| s == "hold") {
        report(Path::new(&args[1]));
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    let executable = std::env::current_exe().unwrap();
    let root = executable.parent().unwrap();
    let probe = args.iter().any(|s| s == "-show_entries");
    report(&root.join(if probe { "probe.ready" } else { "decode.ready" }));
    std::io::stderr()
        .write_all(&vec![b'P'; 256 * 1024])
        .unwrap();
    if probe {
        if std::fs::read_to_string(root.join("mode")).ok().as_deref() == Some("hold-probe") {
            std::thread::sleep(Duration::from_secs(60));
        }
        print!(
            r#"{{"format":{{"duration":"1"}},"streams":[{{"index":0,"codec_type":"video","width":320,"height":240}},{{"index":1,"codec_type":"audio","start_time":"0","duration":"1"}}]}}"#
        );
    } else if args.last().is_some_and(|s| s == "pipe:1") {
        pattern(32000);
    } else {
        let file = args.last().unwrap().replace("%02d", "01");
        std::fs::write(file, [255, 216, 255, 217]).unwrap();
    }
}
