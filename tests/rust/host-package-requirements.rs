use ivy_host_runtime::package_requirements::{
    Requirements, Runtime, runtime_requirements_satisfied,
};

#[test]
fn exact_candidate_platform_and_node_range_never_fall_back() {
    let requirements = Requirements {
        node: ">=24.18.0 <25.0.0",
        os: Some(&["win32", "linux"]),
        arch: Some(&["x64", "arm64"]),
    };
    for (node, expected) in [
        ("v24.18.0", true),
        ("24.19.0", true),
        ("24.17.99", false),
        ("25.0.0", false),
    ] {
        assert_eq!(
            runtime_requirements_satisfied(
                &requirements,
                &Runtime {
                    node,
                    os: "linux",
                    arch: "arm64"
                }
            )
            .unwrap(),
            expected
        );
    }
    assert!(
        !runtime_requirements_satisfied(
            &requirements,
            &Runtime {
                node: "24.18.0",
                os: "darwin",
                arch: "arm64"
            }
        )
        .unwrap()
    );
    assert!(
        !runtime_requirements_satisfied(
            &requirements,
            &Runtime {
                node: "24.18.0",
                os: "win32",
                arch: "ia32"
            }
        )
        .unwrap()
    );
    for node in ["24.018.0", "24.18", "24.18.0-beta", "24.18.0\n"] {
        assert_eq!(
            runtime_requirements_satisfied(
                &requirements,
                &Runtime {
                    node,
                    os: "win32",
                    arch: "x64"
                }
            )
            .unwrap_err()
            .code,
            "unsupported_runtime"
        );
    }
}

#[test]
fn mandatory_node_requirement_keeps_its_existing_exact_grammar() {
    for node in [
        "",
        "*",
        ">=24.18.0",
        ">=24.18.0  <25.0.0",
        ">=24.18.0 <=25.0.0",
    ] {
        assert_eq!(
            runtime_requirements_satisfied(
                &Requirements {
                    node,
                    os: None,
                    arch: None
                },
                &Runtime {
                    node: "24.18.0",
                    os: "linux",
                    arch: "x64"
                }
            )
            .unwrap_err()
            .code,
            "invalid_arguments"
        );
    }
}
