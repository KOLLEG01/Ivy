use ivy_contracts::{
    IvyError, IvyJson, JsString,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_hive::storage_inspector::inspect_admitted_configuration;
use std::io::{self, Read};

fn main() {
    let result = (|| {
        let mut bytes = vec![];
        io::stdin()
            .take(MANAGEMENT_FRAME_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| IvyError::internal())?;
        if bytes.len() > MANAGEMENT_FRAME_BYTES {
            return Err(IvyError::new(
                "invalid_arguments",
                "Admitted configuration exceeds the management frame.",
            ));
        }
        let config = parse(&String::from_utf8_lossy(&bytes), usize::MAX)
            .map_err(|_| IvyError::internal())?;
        let args = std::env::args_os()
            .skip(1)
            .map(|arg| {
                #[cfg(windows)]
                {
                    use std::os::windows::ffi::OsStrExt;
                    JsString(arg.encode_wide().collect())
                }
                #[cfg(not(windows))]
                {
                    JsString::from(arg.to_string_lossy().as_ref())
                }
            })
            .collect::<Vec<_>>();
        inspect_admitted_configuration(&config, &args)
    })();
    match result {
        Ok(value) => println!(
            "{}",
            stringify(&value, usize::MAX).expect("validated storage inspection")
        ),
        Err(error) => {
            let value = IvyJson::object([
                ("code", error.code.into()),
                (
                    "message",
                    "Hive storage inspection failed without mutation.".into(),
                ),
            ]);
            eprintln!(
                "{}",
                stringify(&value, 4096).expect("bounded storage inspection error")
            );
            std::process::exit(1);
        }
    }
}
