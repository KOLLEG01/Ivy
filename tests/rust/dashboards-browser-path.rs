#[path = "../../services/dashboards/rust/src/browser_path.rs"]
mod browser_path;
use browser_path::{BrowserPaths, Platform};
use std::{collections::BTreeMap, path::Path};

#[test]
fn exact_pinned_default_paths_and_no_system_browser_fallback() {
    let root = std::env::current_dir().unwrap();
    let home = root.join("fake-home");
    let package = root.join("artifact/node_modules/playwright-core");
    let env = BTreeMap::new();
    for (platform, arm64, expected) in [
        (
            Platform::Windows,
            false,
            "AppData/Local/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-win64/chrome-headless-shell.exe",
        ),
        (
            Platform::Windows,
            true,
            "AppData/Local/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-win64/chrome-headless-shell.exe",
        ),
        (
            Platform::Linux,
            false,
            ".cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell",
        ),
        (
            Platform::Linux,
            true,
            ".cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux-arm64/chrome-headless-shell",
        ),
        (
            Platform::Mac,
            true,
            "Library/Caches/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-mac-arm64/chrome-headless-shell",
        ),
    ] {
        let paths = BrowserPaths {
            platform,
            arm64,
            home: &home,
            cwd: &root,
            playwright_package: &package,
            environment: &env,
        };
        assert_eq!(paths.resolve(None), Some(home.join(expected)));
        assert_eq!(
            paths.resolve(Some(Path::new("relative/custom-chrome"))),
            Some("relative/custom-chrome".into())
        );
    }
    assert_eq!(browser_path::BROWSER_VERSION, "153.0.8010.12");
}

#[test]
fn preserves_package_cache_relative_env_precedence_and_platform_override() {
    let root = std::env::current_dir().unwrap();
    let package = root.join("artifact/node_modules/playwright-core");
    let suffix = "chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell";
    let mut env = BTreeMap::from([
        ("PLAYWRIGHT_BROWSERS_PATH".into(), "0".into()),
        (
            "npm_config_playwright_browsers_path".into(),
            "ignored".into(),
        ),
    ]);
    let resolve = |env: &BTreeMap<String, String>| {
        BrowserPaths {
            platform: Platform::Linux,
            arm64: false,
            home: &root,
            cwd: &root,
            playwright_package: &package,
            environment: env,
        }
        .resolve(None)
    };
    assert_eq!(
        resolve(&env),
        Some(package.join(".local-browsers").join(suffix))
    );
    env.remove("PLAYWRIGHT_BROWSERS_PATH");
    env.insert(
        "npm_config_playwright_browsers_path".into(),
        "../cache".into(),
    );
    env.insert(
        "INIT_CWD".into(),
        root.join("launch").to_string_lossy().into(),
    );
    assert_eq!(resolve(&env), Some(root.join("cache").join(suffix)));
    env.insert("PLAYWRIGHT_BROWSERS_PATH".into(), "".into());
    assert_eq!(
        resolve(&env),
        Some(root.join(".cache/ms-playwright").join(suffix))
    );
    env.insert(
        "PLAYWRIGHT_HOST_PLATFORM_OVERRIDE".into(),
        "<unknown>".into(),
    );
    assert_eq!(resolve(&env), None);
}
