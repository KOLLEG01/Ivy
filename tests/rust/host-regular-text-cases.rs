use super::*;
#[path = "host-regular-text-support.rs"]
mod support;
use ivy_contracts::IvyJson as J;
use support::*;

#[tokio::test]
async fn regular_text_opened_identity_and_missing_races_match_original() {
    let fixture = Fixture::new();
    for (stage, action) in [
        ("before-open", "remove"),
        ("after-stat", "remove"),
        ("after-stat", "replace"),
        ("after-stat", "same-inode"),
        ("after-stat", "grow"),
        ("before-read", "grow"),
    ] {
        fixture.reset();
        let path = fixture.path("input");
        let root = fixture.root.clone();
        let target = path.clone();
        let callback = Arc::new(move |at: &str| -> io::Result<()> {
            if at != stage {
                return Ok(());
            }
            match action {
                "remove" => fs::remove_file(&target),
                "replace" => {
                    fs::rename(&target, target.with_file_name("input.held"))?;
                    fs::write(&target, b"replacement")
                }
                "same-inode" => {
                    fs::rename(&target, target.with_file_name("input.held"))?;
                    fs::hard_link(target.with_file_name("input.held"), &target)
                }
                "grow" => {
                    use std::io::Write;
                    OpenOptions::new()
                        .append(true)
                        .open(&target)?
                        .write_all(b"-late")
                }
                _ => unreachable!(),
            }
        });
        let actual = value(
            read(
                text(&root),
                text(&path),
                8,
                Stages {
                    callback: Some(callback),
                },
            )
            .await,
        );
        fixture.reset();
        let mut input = request("read", &root, text(&path), 8);
        input.insert("guard", J::String(text(&root)));
        input.insert("stage", stage.into());
        input.insert("action", action.into());
        pair(
            &format!("race-{stage}-{action}"),
            input.clone(),
            actual,
            oracle(&input),
        );
    }
}

#[tokio::test]
async fn regular_text_symlink_swaps_match_platform_original_without_fallback() {
    let fixture = Fixture::new();
    let target = fixture.path("target");
    fs::write(&target, b"replacement").unwrap();
    let probe = fixture.path("probe");
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&target, &probe);
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&target, &probe);
    if let Err(error) = linked {
        blocked("symlink-swap-unavailable", &error);
        return;
    }
    fs::remove_file(probe).unwrap();
    for stage in ["before-open", "after-stat"] {
        fixture.reset();
        let path = fixture.path("input");
        let source = path.clone();
        let destination = target.clone();
        let callback = Arc::new(move |at: &str| -> io::Result<()> {
            if at != stage {
                return Ok(());
            }
            fs::rename(&source, source.with_file_name("input.held"))?;
            #[cfg(windows)]
            {
                std::os::windows::fs::symlink_file(&destination, &source)
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&destination, &source)
            }
        });
        let actual = value(
            read(
                text(&fixture.root),
                text(&path),
                64,
                Stages {
                    callback: Some(callback),
                },
            )
            .await,
        );
        fixture.reset();
        let mut input = request("read", &fixture.root, text(&path), 64);
        input.insert("guard", J::String(text(&fixture.root)));
        input.insert("stage", stage.into());
        input.insert("action", "symlink".into());
        input.insert("target", J::String(text(&target)));
        pair(
            &format!("race-{stage}-symlink"),
            input.clone(),
            actual,
            oracle(&input),
        );
    }
}
