use ivy_contracts::JsString;
use ivy_host_runtime::{
    emitted_runtime::emitted_runtime_packages, filesystem, node_runtime::NodeRuntime, process,
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-emitted-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let file = self.0.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
    }
    fn compiler(&self) {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("node_modules/typescript");
        let modules = self.0.join("node_modules");
        fs::create_dir_all(&modules).unwrap();
        filesystem::directory_link(&source, &modules.join("typescript")).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = filesystem::remove_force_sync(&self.0);
    }
}
fn node() -> NodeRuntime {
    let executable = std::env::var_os("IVY_TEST_NODE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let output = std::process::Command::new("node")
                .args(["-p", "process.execPath"])
                .output()
                .unwrap();
            assert!(output.status.success());
            PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        });
    NodeRuntime {
        executable,
        version: "24.18.0".into(),
    }
}
async fn inventory(root: &Path) -> ivy_host_runtime::IvyResult<Vec<JsString>> {
    emitted_runtime_packages(
        root,
        &node(),
        process::LaunchContext {
            job_launcher: Some(PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))),
            ..Default::default()
        },
    )
    .await
}
#[tokio::test]
async fn actual_compiler_inventories_literal_imports_and_skips_non_runtime_syntax() {
    let fixture = Fixture::new();
    fixture.compiler();
    fixture.write("dist/root.cjs",br#"import x from 'alpha/sub'; export {a} from '@scope/leaf/path'; require('beta'); import('gamma');
    function nested(require) { require(`template/sub`); }
    // require('ignored-comment');
    const text="require('ignored-string')"; require(variable); other.require('ignored-property');
    require('node:fs');require('./relative');require('/absolute');require('bad:name');require('@incomplete');
    require('ALPHA');require('alpha/again');require('_leading');require('with.dot/path');require('@s/p');
    require('bad!');require('p/\ud800');require('\ud800bad');require('@a/');require('');require('.hidden');"#);
    fixture.write("dist/ignored.JS", b"require('wrong-extension')");
    fixture.write("dist/ignored.js.map", b"require('wrong-map')");
    fixture.write(
        "dist/nested/second.mjs",
        b"export * from 'deep';require('beta/sub');import('dynamic');",
    );
    fixture.write(
        "dist/nested/third.js",
        b"const broken = ; require('recovered'); import x from 'last';",
    );
    fixture.write("dist/directory.js/child.js", b"require('directory-child')");
    fixture.write("dist/unicode-\u{1f600}.js", b"require('unicode-file')");
    fixture.write("dist/replace.js", b"// \xff\nrequire('decoded-source')");
    let packages = inventory(&fixture.0).await.unwrap();
    let expected: Vec<JsString> = [
        "alpha",
        "@scope/leaf",
        "beta",
        "gamma",
        "template",
        "ALPHA",
        "_leading",
        "with.dot",
        "@s/p",
        "p",
        "deep",
        "dynamic",
        "recovered",
        "last",
        "directory-child",
        "unicode-file",
        "decoded-source",
    ]
    .into_iter()
    .map(JsString::from)
    .collect();
    assert_eq!(packages.len(), expected.len());
    assert_eq!(
        packages.iter().cloned().collect::<BTreeSet<_>>(),
        expected.into_iter().collect()
    );
    let index = |name: &str| {
        packages
            .iter()
            .position(|p| p == &JsString::from(name))
            .unwrap()
    };
    assert!(index("alpha") < index("beta") && index("beta") < index("gamma"));
    assert!(index("gamma") < index("template"));
    assert!(index("unicode-file") < index("directory-child"));
    assert!(index("unicode-file") < index("deep"));
}
#[tokio::test]
async fn actual_compiler_skips_directory_links_and_refuses_unavailable_output() {
    let fixture = Fixture::new();
    fixture.compiler();
    fixture.write("outside/hidden.js", b"require('outside')");
    fixture.write("dist/ordinary.js", b"require('inside')");
    filesystem::directory_link(
        &fixture.0.join("outside"),
        &fixture.0.join("dist/linked-directory"),
    )
    .unwrap();
    assert_eq!(
        inventory(&fixture.0).await.unwrap(),
        vec![JsString::from("inside")]
    );
    assert_eq!(
        fs::read(fixture.0.join("outside/hidden.js")).unwrap(),
        b"require('outside')"
    );
    filesystem::remove_force_sync(&fixture.0.join("dist")).unwrap();
    assert_eq!(
        inventory(&fixture.0).await.unwrap_err().code,
        "command_failed"
    );
    fixture.write("dist", b"plain file");
    assert_eq!(
        inventory(&fixture.0).await.unwrap_err().code,
        "command_failed"
    );
}
#[tokio::test]
async fn parser_requires_the_prepared_sources_compiler_instead_of_an_ambient_package() {
    let fixture = Fixture::new();
    fixture.compiler();
    fixture.write("source/dist/main.js", b"require('inside')");
    // The parent has a real compiler; the selected source deliberately does not.
    assert_eq!(
        inventory(&fixture.0.join("source")).await.unwrap_err().code,
        "command_failed"
    );
}
