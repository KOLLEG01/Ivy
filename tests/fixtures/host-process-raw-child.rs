use std::{collections::BTreeMap, io::{Read, Write}, time::Duration};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "fail") { std::process::exit(7); }
    if args.get(1).is_some_and(|arg| arg == "sleep") {
        std::io::stdout().write_all(b"R").unwrap();
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    let environment: BTreeMap<_,_> = std::env::vars_os().collect();
    let mut stdin = Vec::new();
    std::io::stdin().read_to_end(&mut stdin).unwrap();
    std::fs::write(&args[2],format!("{environment:?}\nstdin={}\n",stdin.len())).unwrap();
    let amount: usize = args[3].parse().unwrap();
    let bytes: Vec<_> = (0..amount).map(|index| (index % 256) as u8).collect();
    std::io::stdout().write_all(&bytes).unwrap();
    std::io::stderr().write_all(&vec![b'p'; 256 * 1024]).unwrap();
}
