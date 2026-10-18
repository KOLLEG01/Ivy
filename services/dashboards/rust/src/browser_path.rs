//! Pinned playwright-core 1.63.0 headless-shell cache layout. Discovery never
//! downloads a runtime and never searches PATH or an interactive browser profile.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub const BROWSER_REVISION: &str = "1243";
pub const BROWSER_VERSION: &str = "153.0.8010.12";

#[derive(Clone, Copy, Debug)]
pub enum Platform {
    Windows,
    Linux,
    Mac,
}

pub struct BrowserPaths<'a> {
    pub platform: Platform,
    pub arm64: bool,
    pub home: &'a Path,
    pub cwd: &'a Path,
    /// Original package location within the immutable artifact, also used when
    /// PLAYWRIGHT_BROWSERS_PATH=0. This may contain a provisioned local cache.
    pub playwright_package: &'a Path,
    pub environment: &'a BTreeMap<String, String>,
}

impl BrowserPaths<'_> {
    fn env(&self, name: &str) -> Option<&str> {
        self.environment
            .get(name)
            .or_else(|| {
                self.environment
                    .get(&format!("npm_config_{}", name.to_lowercase()))
            })
            .or_else(|| {
                self.environment
                    .get(&format!("npm_package_config_{}", name.to_lowercase()))
            })
            .map(String::as_str)
    }

    pub fn resolve(&self, explicit: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = explicit {
            return Some(path.to_path_buf());
        }
        let cache = match self.env("PLAYWRIGHT_BROWSERS_PATH") {
            Some("0") => self.playwright_package.join(".local-browsers"),
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => match self.platform {
                Platform::Windows => self
                    .environment
                    .get("LOCALAPPDATA")
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.home.join("AppData/Local")),
                Platform::Linux => self
                    .environment
                    .get("XDG_CACHE_HOME")
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.home.join(".cache")),
                Platform::Mac => self.home.join("Library/Caches"),
            }
            .join("ms-playwright"),
        };
        let cache = if cache.is_absolute() {
            cache
        } else {
            let base = self
                .env("INIT_CWD")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| self.cwd.to_path_buf());
            // Node path.resolve also anchors a relative INIT_CWD at process.cwd().
            let base = if base.is_absolute() {
                base
            } else {
                self.cwd.join(base)
            };
            base.join(cache)
        };
        // Preserve Playwright's host-platform override (including unknown), used
        // by provisioned installations and the platform compatibility fixtures.
        let host = self
            .environment
            .get("PLAYWRIGHT_HOST_PLATFORM_OVERRIDE")
            .filter(|s| !s.is_empty());
        let short = match host.map(String::as_str) {
            Some("<unknown>") => return None,
            Some("win64") => "win64",
            Some(value) if value.starts_with("mac") => {
                if value.ends_with("arm64") {
                    "mac-arm64"
                } else {
                    "mac-x64"
                }
            }
            Some(value) => {
                if value.ends_with("arm64") {
                    "linux-arm64"
                } else {
                    "linux64"
                }
            }
            None => match self.platform {
                // The pinned upstream package resolves Windows ARM to its x64
                // headless shell; retaining that mapping is not ARM qualification.
                Platform::Windows => "win64",
                Platform::Linux => {
                    if self.arm64 {
                        "linux-arm64"
                    } else {
                        "linux64"
                    }
                }
                Platform::Mac => {
                    if self.arm64 {
                        "mac-arm64"
                    } else {
                        "mac-x64"
                    }
                }
            },
        };
        let executable = if short == "win64" {
            "chrome-headless-shell.exe"
        } else {
            "chrome-headless-shell"
        };
        Some(normalize(
            cache
                .join(format!("chromium_headless_shell-{BROWSER_REVISION}"))
                .join(format!("chrome-headless-shell-{short}"))
                .join(executable),
        ))
    }
}

fn normalize(path: PathBuf) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component.as_os_str()),
        }
    }
    result
}
