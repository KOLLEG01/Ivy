use ivy_cli::reset_ui::ResetUiBundle;
use ivy_contracts::{IvyError, IvyJson as J, JsString, Outcome, json::{parse, stringify}};
use std::{path::PathBuf, sync::Mutex};
#[path = "cli-reset-ui-auth.rs"]
mod auth;
fn json(text: &str) -> J { parse(text, usize::MAX).unwrap() }
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let parent = std::fs::canonicalize(
            std::env::var_os("IVY_TEST_TEMP")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir),
        )
        .unwrap();
        let path = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(path.parent(), Some(parent.as_path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-reset-ui-")
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
