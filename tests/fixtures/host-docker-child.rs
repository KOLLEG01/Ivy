use std::{env,fs,io::{self,Write}};
fn main(){
    let root=env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let args:Vec<_>=env::args().skip(1).collect();
    assert_eq!(&args[..2],&["--host","unix:///var/run/docker.sock"]);
    let key=format!("{}-{}",args[2],args[3]);
    let mut trace=fs::OpenOptions::new().create(true).append(true).open(root.join("native-commands.txt")).unwrap();writeln!(trace,"{args:?}").unwrap();
    if root.join(format!("{key}.fail")).exists(){io::stderr().write_all(b"private command failure").unwrap();std::process::exit(7);}
    let path=root.join(format!("{key}.stdout"));if path.exists(){io::stdout().write_all(&fs::read(path).unwrap()).unwrap();}
    let path=root.join(format!("{key}.stderr"));if path.exists(){io::stderr().write_all(&fs::read(path).unwrap()).unwrap();}
}
