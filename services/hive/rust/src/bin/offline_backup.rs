use ivy_contracts::{IvyError, IvyJson, json::stringify};
use ivy_hive::storage_read::snapshot_stopped_hive;
use std::path::Path;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.len() < 2 || args[0].is_empty() || args[1].is_empty() {
        Err(IvyError::new(
            "invalid_arguments",
            "Specify the offline source and destination.",
        ))
    } else {
        snapshot_stopped_hive(Path::new(&args[0]), Path::new(&args[1]))
    };
    match result {
        Ok(value) => println!(
            "{}",
            stringify(&value, 1024).expect("bounded backup result")
        ),
        Err(error) => {
            let value = IvyJson::Object(vec![
                ("code".into(), error.code.into()),
                ("message".into(), "Offline Hive snapshot failed.".into()),
            ]);
            eprintln!("{}", stringify(&value, 4096).expect("bounded backup error"));
            std::process::exit(1);
        }
    }
}
