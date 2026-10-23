// A standalone receiver is needed because libtest converts argv to UTF-8 before
// running a test and panics on the isolated UTF-16 units under qualification.
#[cfg(windows)]
fn main() {
    use std::os::windows::ffi::OsStrExt;
    let args: Vec<Vec<u16>> = std::env::args_os().map(|value| value.encode_wide().collect()).collect();
    std::fs::write(std::env::var_os("IVY_EXEC_FILE_ARGUMENTS").unwrap(), format!("{args:?}")).unwrap();
    if let Some(receipt) = std::env::var_os("IVY_EXEC_FILE_CWD") {
        let cwd: Vec<u16> = std::env::current_dir().unwrap().as_os_str().encode_wide().collect();
        std::fs::write(receipt, format!("{cwd:?}")).unwrap();
    }
}
#[cfg(not(windows))]
fn main() {}
