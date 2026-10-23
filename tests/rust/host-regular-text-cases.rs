use super::*;
#[path = "host-regular-text-support.rs"]
mod support;
use support::*;

#[test]
fn regular_text_opened_identity_missing_and_growth_races() {
    for (stage, action) in [
        ("before-open", "remove"),
        ("after-stat", "remove"),
        ("after-stat", "replace"),
        ("after-stat", "same-inode"),
        ("after-stat", "grow"),
        ("before-read", "grow"),
    ] {
        let fixture = Fixture::new();
        fixture.reset();
        let path = fixture.path("input");
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
        let result = optional(read(
            &Context::current().unwrap(),
            &text(&fixture.root),
            &text(&path),
            8,
            Stages {
                callback: Some(callback),
            },
        ));
        match action {
            "remove" => assert!(result.unwrap().is_none(), "{stage}"),
            "same-inode" => assert_eq!(result.unwrap().unwrap(), JsString::from("retained")),
            "replace" => assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("changed during validation")
            ),
            "grow" => assert!(
                result.unwrap_err().to_string().contains("size limit"),
                "{stage}"
            ),
            _ => unreachable!(),
        }
    }
}

#[test]
fn regular_text_rejects_parent_directory_link_swaps() {
    for stage in ["before-open", "after-stat"] {
        let fixture = Fixture::new();
        let owned = fixture.path("owned");
        let link = fixture.path("link");
        let outside = fixture.path("outside");
        fs::create_dir(&owned).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(owned.join("input"), b"retained").unwrap();
        fs::write(outside.join("input"), b"escaped").unwrap();
        crate::filesystem::directory_link(&owned, &link).unwrap();
        let source = link.clone();
        let target = outside.clone();
        let callback = Arc::new(move |at: &str| -> io::Result<()> {
            if at != stage {
                return Ok(());
            }
            #[cfg(windows)]
            fs::remove_dir(&source)?;
            #[cfg(unix)]
            fs::remove_file(&source)?;
            crate::filesystem::directory_link(&target, &source)
        });
        let result = read(
            &Context::current().unwrap(),
            &text(&owned),
            &text(&link.join("input")),
            64,
            Stages {
                callback: Some(callback),
            },
        );
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("changed during validation"),
            "{stage}: {error}"
        );
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        fs::remove_file(&link).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn regular_text_rejects_file_symlink_swaps() {
    for stage in ["before-open", "after-stat"] {
        let fixture = Fixture::new();
        fixture.reset();
        let target = fixture.path("target");
        fs::write(&target, b"replacement").unwrap();
        let path = fixture.path("input");
        let source = path.clone();
        let callback = Arc::new(move |at: &str| -> io::Result<()> {
            if at != stage {
                return Ok(());
            }
            fs::rename(&source, source.with_file_name("input.held"))?;
            std::os::unix::fs::symlink(&target, &source)
        });
        assert!(
            read(
                &Context::current().unwrap(),
                &text(&fixture.root),
                &text(&path),
                64,
                Stages {
                    callback: Some(callback)
                },
            )
            .is_err(),
            "{stage}"
        );
    }
}
