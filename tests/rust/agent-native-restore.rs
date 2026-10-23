//! Restore fresh copies through the current native Host APIs and retained layouts.
use ivy_contracts::{
    IvyJson as J, JsString,
    hash::{digest, hash_json},
    json::{parse, stringify},
};
use ivy_host_runtime::{
    backup_files,
    layout::StorageContext,
    native_backup_paths::NativeBackupPolicy,
    native_restore,
    node_path::{self, Context},
    paths,
};
use ivy_store::rusqlite::{Connection, params};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = paths::realpath(&std::env::temp_dir()).unwrap();
        let path = parent.join(format!("ivy-agent-native-restore-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained native restore fixture: {}", self.0.display());
            return;
        }
        let actual = paths::realpath(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(paths::realpath(&std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-agent-native-restore-")
        );
        fs::remove_dir_all(actual).unwrap();
    }
}
fn field<'a>(value: &'a J, key: &str) -> &'a J {
    value.required(key).unwrap()
}
fn text(value: &J, key: &str) -> String {
    field(value, key).as_str().unwrap()
}
fn path(value: &Path) -> J {
    J::String(node_path::from_native(value))
}
fn read_json(path: &Path) -> J {
    parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
}
fn write_json(path: &Path, value: &J) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, stringify(value, usize::MAX).unwrap()).unwrap();
}
async fn snapshot(root: &Path) -> BTreeMap<JsString, String> {
    backup_files::inventory_files(root.to_path_buf(), None)
        .await
        .unwrap()
        .into_iter()
        .map(|file| (file.path, file.hash))
        .collect()
}

struct Fixture {
    root: Root,
    layout: J,
    original: J,
    target: J,
    manifest: J,
    old_home: PathBuf,
    new_home: PathBuf,
    distribution: PathBuf,
}
impl Fixture {
    async fn new(version: &str) -> Self {
        let root = Root::new();
        let source = root.0.join("source");
        let target_root = root.0.join("target");
        let old_home = source.join("instances/agent/data/native-home");
        let new_home = target_root.join("instances/agent/data/native-home");
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let directory = repository.join(format!("specs/native/codex-{version}"));
        let platform = if cfg!(windows) {
            "win32-x64"
        } else {
            "linux-x64"
        };
        let platform_layout = directory.join(format!("storage.{platform}.json"));
        let layout = read_json(&if platform_layout.exists() {
            platform_layout
        } else {
            directory.join("storage.json")
        });
        let distribution = root.0.join("distribution");
        write_json(
            &distribution.join(format!("dist/specs/native/codex-{version}/storage.json")),
            &layout,
        );
        for (name, contents) in [
            (
                "sessions/legacy.jsonl",
                "{\"id\":\"legacy\",\"unchanged\":true}\n",
            ),
            (
                "sessions/selected.jsonl",
                "{\"id\":\"selected\",\"parent_rollout_id\":\"legacy\"}\n",
            ),
            (
                "archived_sessions/archived.jsonl",
                "{\"id\":\"archived\"}\n",
            ),
            ("jobs/input.csv", "id,value\n1,original\n"),
            ("goals_1.sqlite", "opaque database; never rewrite"),
            ("config.toml", "user_setting = 'preserve'\n"),
        ] {
            let file = old_home.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, contents).unwrap();
        }
        let db = Connection::open(old_home.join("state_5.sqlite")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0; CREATE TABLE retired_logs(id INTEGER PRIMARY KEY AUTOINCREMENT); DROP TABLE retired_logs;").unwrap();
        let schema = field(&layout, "schema").as_array().unwrap();
        for entry in schema
            .iter()
            .filter(|e| text(e, "type") == "table" && text(e, "name") != "sqlite_sequence")
        {
            db.execute_batch(&text(entry, "sql")).unwrap();
        }
        for entry in schema.iter().filter(|e| text(e, "type") != "table") {
            db.execute_batch(&text(entry, "sql")).unwrap();
        }
        db.execute_batch("CREATE TABLE _sqlx_migrations(version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TEXT NOT NULL, success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)").unwrap();
        for migration in field(&layout, "migrations").as_array().unwrap() {
            let checksum = text(migration, "checksum");
            let checksum: Vec<u8> = (0..checksum.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&checksum[i..i + 2], 16).unwrap())
                .collect();
            db.execute(
                "INSERT INTO _sqlx_migrations VALUES (?,?,?,?,?,?)",
                params![
                    field(migration, "version").as_f64().unwrap() as i64,
                    text(migration, "description"),
                    "fixed-install-time",
                    1,
                    checksum,
                    9_007_199_254_740_993i64
                ],
            )
            .unwrap();
        }
        for (id, local, archived, mode) in [
            ("legacy", "sessions/legacy.jsonl", 0, "legacy"),
            ("selected", "sessions/selected.jsonl", 0, "paginated"),
            ("archived", "archived_sessions/archived.jsonl", 1, "legacy"),
        ] {
            db.execute("INSERT INTO threads(id,rollout_path,created_at,updated_at,source,model_provider,cwd,title,sandbox_policy,approval_mode,archived,history_mode,tokens_used,agent_path) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![id, old_home.join(local).to_str().unwrap(), 100, 200, "cli", "provider",
                    old_home.to_str().unwrap(), "literal title", "{}", "never", archived, mode,
                    9_007_199_254_740_993i64, "/root/child"]).unwrap();
        }
        db.execute(
            "INSERT INTO thread_spawn_edges VALUES (?,?,?)",
            params!["legacy", "selected", "completed"],
        )
        .unwrap();
        if schema.iter().any(|entry| text(entry, "name") == "projects") {
            db.execute_batch("INSERT INTO projects VALUES ('project','Retained project','{}',1,100,200); INSERT INTO thread_sections VALUES ('section','Retained section','blue')").unwrap();
            db.execute(
                "INSERT INTO project_roots VALUES (?,?,?)",
                params!["project", 0, old_home.to_str().unwrap()],
            )
            .unwrap();
            db.execute_batch("UPDATE threads SET name='Named thread',is_pinned=1,project_id='project',thread_section_id='section',section_position=7,section_entered_at_ms=300 WHERE id='selected'").unwrap();
            db.execute(
                "INSERT INTO rollout_migration_skipped_rollouts VALUES (?,?,?,?,?,?)",
                params![
                    "migration",
                    old_home
                        .join("archived_sessions/no-longer-present.jsonl")
                        .to_str()
                        .unwrap(),
                    50,
                    9_007_199_254_740_993i64,
                    "retained skip reason",
                    400
                ],
            )
            .unwrap();
            let attachment = if schema
                .iter()
                .any(|entry| text(entry, "name") == "thread_attachments")
            {
                "thread_attachments"
            } else {
                "thread_artifacts"
            };
            if schema.iter().any(|entry| text(entry, "name") == attachment) {
                db.execute(
                    &format!("INSERT INTO {attachment} VALUES (?,?,?,?,?,?)"),
                    params![
                        "artifact",
                        "selected",
                        "fixture",
                        "original-identity",
                        stringify(
                            &J::object([(
                                "originalPath",
                                path(&old_home.join("sessions/selected.jsonl"))
                            )]),
                            usize::MAX
                        )
                        .unwrap(),
                        9_007_199_254_740_993i64,
                    ],
                )
                .unwrap();
            }
        } else {
            for (id, input, output) in [
                (
                    "owned",
                    old_home.join("jobs/input.csv"),
                    old_home.join("jobs/not-created.csv"),
                ),
                (
                    "external",
                    root.0.join("project/input.csv"),
                    root.0.join("project/output.csv"),
                ),
            ] {
                db.execute("INSERT INTO agent_jobs(id,name,status,instruction,input_headers_json,input_csv_path,output_csv_path,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
                    params![id, "Saved job", "completed", old_home.to_str().unwrap(), "[]", input.to_str().unwrap(), output.to_str().unwrap(), 100, 200]).unwrap();
            }
        }
        fs::write(
            old_home.join("config.toml"),
            format!(
                "historical_value = {}\n",
                stringify(&path(&old_home), usize::MAX).unwrap()
            ),
        )
        .unwrap();
        let settings = J::object([
            ("appServer", J::object([("mode", "owned-stdio".into())])),
            ("nativeHome", path(&old_home)),
            ("nativeVersion", version.into()),
            ("nativeExecutableHash", digest("fixture-executable").into()),
        ]);
        let instance = J::object([
            ("instanceId", "agent".into()),
            ("serviceNodeId", "restore.agent".into()),
            ("componentId", "agent-manager".into()),
            ("enabled", false.into()),
            ("engine", "process".into()),
            ("settings", settings),
        ]);
        let original = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", "restore".into()),
            ("runtimeRoot", path(&source)),
            ("artifactRoot", path(&root.0.join("artifacts"))),
            ("stagingRoot", path(&root.0.join("staging"))),
            ("publicBaseUrl", "http://127.0.0.1:38901".into()),
            ("executables", J::Object(Vec::new())),
            ("instances", J::Array(vec![instance.clone()])),
        ]);
        let mut target = original.clone();
        target.insert("runtimeRoot", path(&target_root));
        target.insert(
            "restoredFrom",
            J::object([
                ("restoreId", "fresh-copy".into()),
                ("backupId", "fixture-backup".into()),
                ("manifestHash", digest("fixture-manifest").into()),
            ]),
        );
        let mut restored_instance = instance;
        let mut settings = field(&restored_instance, "settings").clone();
        settings.insert("nativeHome", path(&new_home));
        restored_instance.insert("settings", settings);
        target.insert("instances", J::Array(vec![restored_instance]));
        // The fixture writer is idle during capture. Only the copied database is restored.
        let files = backup_files::inventory_files(source.clone(), None)
            .await
            .unwrap();
        assert!(
            files
                .iter()
                .any(|f| f.path.to_utf8().unwrap().ends_with("-wal") && f.bytes > 0)
        );
        let mut captured = Vec::new();
        for file in files {
            let local = node_path::filesystem_path(&file.path);
            for destination in [root.0.join("backup").join(&local), target_root.join(&local)] {
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                fs::copy(source.join(&local), destination).unwrap();
            }
            captured.push(J::object([
                (
                    "path",
                    format!("state/{}", file.path.to_utf8().unwrap().replace('\\', "/")).into(),
                ),
                ("hash", file.hash.into()),
                ("bytes", J::Number(file.bytes as f64)),
                ("mode", J::Number(file.mode as f64)),
            ]));
        }
        drop(db);
        let manifest = J::object([
            ("backupId", "fixture-backup".into()),
            ("files", J::Array(captured)),
        ]);
        Self {
            root,
            layout,
            original,
            target,
            manifest,
            old_home,
            new_home,
            distribution,
        }
    }
    fn assert_retained_relational_data(&self, db: &Connection) {
        let has_table = |name| {
            field(&self.layout, "schema")
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| text(entry, "name") == name)
        };
        assert_eq!(
            db.query_row(
                "SELECT execution_time FROM _sqlx_migrations LIMIT 1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            9_007_199_254_740_993
        );
        if has_table("projects") {
            assert_eq!(db.query_row("SELECT project_id,thread_section_id,section_position,is_pinned FROM threads WHERE id='selected'", [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))).unwrap(),
                ("project".into(), "section".into(), 7, 1));
            assert_eq!(
                db.query_row("SELECT path FROM project_roots", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                self.old_home.to_str().unwrap()
            );
            assert_eq!(
                db.query_row(
                    "SELECT rollout_path FROM rollout_migration_skipped_rollouts",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                self.new_home
                    .join("archived_sessions/no-longer-present.jsonl")
                    .to_str()
                    .unwrap()
            );
            let table = if has_table("thread_attachments") {
                "thread_attachments"
            } else {
                "thread_artifacts"
            };
            if has_table(table) {
                let (payload, created): (String, i64) = db
                    .query_row(
                        &format!("SELECT payload,created_at FROM {table}"),
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(
                    field(&parse(&payload, usize::MAX).unwrap(), "originalPath"),
                    &path(&self.old_home.join("sessions/selected.jsonl"))
                );
                assert_eq!(created, 9_007_199_254_740_993);
            }
        } else {
            for (key, expected) in [
                ("input_csv_path", self.new_home.join("jobs/input.csv")),
                (
                    "output_csv_path",
                    self.new_home.join("jobs/not-created.csv"),
                ),
            ] {
                assert_eq!(
                    db.query_row(
                        &format!("SELECT {key} FROM agent_jobs WHERE id='owned'"),
                        [],
                        |r| r.get::<_, String>(0)
                    )
                    .unwrap(),
                    expected.to_str().unwrap()
                );
            }
            assert_eq!(
                db.query_row(
                    "SELECT input_csv_path FROM agent_jobs WHERE id='external'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                self.root.0.join("project/input.csv").to_str().unwrap()
            );
            assert_eq!(
                db.query_row(
                    "SELECT instruction FROM agent_jobs WHERE id='owned'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                self.old_home.to_str().unwrap()
            );
        }
    }
    fn database(&self) -> PathBuf {
        self.new_home.join("state_5.sqlite")
    }
    fn policy(&self) -> NativeBackupPolicy {
        NativeBackupPolicy::new(
            StorageContext {
                paths: Context::current().unwrap(),
                account_home: node_path::from_native(&self.root.0),
                compare: Arc::new(|a, b| a.cmp(b)),
            },
            BTreeMap::new(),
            Context::current().unwrap().cwd,
        )
    }
    async fn restore(&self) -> ivy_contracts::IvyResult<J> {
        native_restore::relocate_native_storage(
            &self.original,
            &self.target,
            &self.manifest,
            &self.distribution,
            &self.policy(),
        )
        .await
    }
    async fn reseal_database(&mut self) {
        let files = backup_files::inventory_files(self.root.0.join("target"), None)
            .await
            .unwrap();
        let mut captured: Vec<_> = field(&self.manifest, "files")
            .as_array()
            .unwrap()
            .iter()
            .filter(|file| !text(file, "path").contains("/state_5.sqlite"))
            .cloned()
            .collect();
        for file in files
            .into_iter()
            .filter(|f| f.path.to_utf8().unwrap().contains("state_5.sqlite"))
        {
            captured.push(J::object([
                (
                    "path",
                    format!("state/{}", file.path.to_utf8().unwrap().replace('\\', "/")).into(),
                ),
                ("hash", file.hash.into()),
                ("bytes", J::Number(file.bytes as f64)),
                ("mode", J::Number(file.mode as f64)),
            ]));
        }
        self.manifest.insert("files", J::Array(captured));
    }
}

#[tokio::test]
async fn copied_native_restore_preserves_wal_lineage_bigints_and_publishes_verified_report() {
    for version in ["0.154.0", "0.158.0", "0.159.2"] {
        let fixture = Fixture::new(version).await;
        let source_before = snapshot(&fixture.root.0.join("source")).await;
        let backup_before = snapshot(&fixture.root.0.join("backup")).await;
        let report = fixture.restore().await.unwrap();
        let instances = field(&report, "instances").as_array().unwrap();
        assert_eq!(instances.len(), 1, "{version}");
        let instance = &instances[0];
        assert_eq!(
            text(instance, "layoutHash"),
            hash_json(&fixture.layout).unwrap()
        );
        assert_eq!(
            text(instance, "workingHash"),
            digest(fs::read(fixture.database()).unwrap())
        );
        assert_eq!(
            hash_json(&read_json(
                &fixture.root.0.join("target/native-recovery.json")
            ))
            .unwrap(),
            hash_json(&report).unwrap()
        );
        chrono::DateTime::parse_from_rfc3339(&text(&report, "completedAt")).unwrap();
        let original_files = field(instance, "originalFiles").as_array().unwrap();
        assert!(
            original_files
                .iter()
                .any(|file| text(file, "path").ends_with("-wal")
                    && field(file, "bytes").as_f64().unwrap() > 0.0)
        );
        for file in original_files {
            assert_eq!(
                digest(fs::read(fixture.root.0.join("target").join(text(file, "path"))).unwrap()),
                text(file, "hash")
            );
        }
        let db = Connection::open(fixture.database()).unwrap();
        let mut query = db.prepare("SELECT id,rollout_path,cwd,title,tokens_used,history_mode FROM threads ORDER BY id").unwrap();
        let rows = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .unwrap();
        for row in rows {
            let (id, rollout, cwd, title, tokens, history) = row.unwrap();
            let local = match id.as_str() {
                "archived" => "archived_sessions/archived.jsonl",
                "legacy" => "sessions/legacy.jsonl",
                _ => "sessions/selected.jsonl",
            };
            assert_eq!(rollout, fixture.new_home.join(local).to_str().unwrap());
            assert_eq!(cwd, fixture.old_home.to_str().unwrap());
            assert_eq!(title, "literal title");
            assert_eq!(tokens, 9_007_199_254_740_993);
            assert_eq!(
                history,
                if id == "selected" {
                    "paginated"
                } else {
                    "legacy"
                }
            );
            assert_eq!(
                fs::read(fixture.new_home.join(local)).unwrap(),
                fs::read(fixture.old_home.join(local)).unwrap()
            );
        }
        assert_eq!(
            db.query_row(
                "SELECT parent_thread_id,child_thread_id,status FROM thread_spawn_edges",
                [],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?
                ))
            )
            .unwrap(),
            ("legacy".into(), "selected".into(), "completed".into())
        );
        fixture.assert_retained_relational_data(&db);
        drop(query);
        drop(db);
        assert_eq!(
            fs::read(fixture.new_home.join("goals_1.sqlite")).unwrap(),
            b"opaque database; never rewrite"
        );
        assert_eq!(
            fs::read(fixture.new_home.join("config.toml")).unwrap(),
            fs::read(fixture.old_home.join("config.toml")).unwrap()
        );
        assert_eq!(
            snapshot(&fixture.root.0.join("source")).await,
            source_before
        );
        assert_eq!(
            snapshot(&fixture.root.0.join("backup")).await,
            backup_before
        );
    }
}

#[tokio::test]
async fn native_restore_refuses_changed_layout_pointers_files_and_build_without_success_report() {
    for (mutation, code) in [
        ("checksum", "unsupported_storage"),
        ("schema", "unsupported_storage"),
        ("missing-pointer", "backup_invalid"),
        ("outside-pointer", "backup_invalid"),
        ("changed-file", "backup_changed"),
        ("build", "target_conflict"),
    ] {
        let mut fixture = Fixture::new("0.154.0").await;
        let source_before = snapshot(&fixture.root.0.join("source")).await;
        let backup_before = snapshot(&fixture.root.0.join("backup")).await;
        if ["checksum", "schema", "missing-pointer", "outside-pointer"].contains(&mutation) {
            let db = Connection::open(fixture.database()).unwrap();
            match mutation {
                "checksum" => db
                    .execute_batch("UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=1")
                    .unwrap(),
                "schema" => db
                    .execute_batch("CREATE TABLE unexpected(value TEXT)")
                    .unwrap(),
                _ => {
                    db.execute(
                        "UPDATE threads SET rollout_path=? WHERE id='selected'",
                        [if mutation == "missing-pointer" {
                            fixture.old_home.join("sessions/absent.jsonl")
                        } else {
                            fixture.root.0.join("outside.jsonl")
                        }
                        .to_str()
                        .unwrap()],
                    )
                    .unwrap();
                }
            }
            drop(db);
            fixture.reseal_database().await;
        } else if mutation == "changed-file" {
            fs::write(fixture.new_home.join("sessions/selected.jsonl"), "changed").unwrap();
        } else {
            let mut instance = field(&fixture.target, "instances").as_array().unwrap()[0].clone();
            let mut settings = field(&instance, "settings").clone();
            settings.insert("nativeExecutableHash", digest("different-build").into());
            instance.insert("settings", settings);
            fixture.target.insert("instances", J::Array(vec![instance]));
        }
        assert_eq!(
            fixture.restore().await.unwrap_err().code,
            code,
            "{mutation}"
        );
        assert!(
            !fixture.root.0.join("target/native-recovery.json").exists(),
            "{mutation}"
        );
        assert_eq!(
            snapshot(&fixture.root.0.join("source")).await,
            source_before
        );
        assert_eq!(
            snapshot(&fixture.root.0.join("backup")).await,
            backup_before
        );
    }
}

#[tokio::test]
async fn pointer_mapper_and_failed_transaction_preserve_paths_and_database_rows() {
    let fixture = Fixture::new("0.154.0").await;
    let before = snapshot(&fixture.root.0.join("target")).await;
    let db = Connection::open(fixture.database()).unwrap();
    let original_path: String = db
        .query_row(
            "SELECT rollout_path FROM threads WHERE id='selected'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut calls = 0;
    let error = native_restore::relocate_database(db, &fixture.layout, |_, _| {
        calls += 1;
        if calls == 1 {
            Ok(Some("changed".into()))
        } else {
            Err(ivy_contracts::IvyError::new(
                "backup_invalid",
                "Fixture refuses the next pointer",
            ))
        }
    })
    .unwrap_err();
    assert_eq!(error.code, "backup_invalid");
    let db = Connection::open(fixture.database()).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT rollout_path FROM threads WHERE id='selected'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        original_path
    );
    drop(db);
    let context = Context::current().unwrap();
    let files = field(&fixture.manifest, "files")
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (field(f, "path").as_js_string().unwrap().clone(), f.clone()))
        .collect();
    let map = |raw: &Path, scope| {
        native_restore::mapped_file(
            &node_path::from_native(raw),
            scope,
            &node_path::from_native(&fixture.old_home),
            &node_path::from_native(&fixture.new_home),
            &"state/instances/agent/data/native-home".into(),
            &files,
            &context,
        )
    };
    assert_eq!(
        map(
            &fixture.old_home.join("sessions/selected.jsonl"),
            "required-rollout"
        )
        .unwrap(),
        Some(node_path::from_native(
            &fixture.new_home.join("sessions/selected.jsonl")
        ))
    );
    assert_eq!(
        map(&fixture.root.0.join("external.csv"), "optional-owned-file").unwrap(),
        None
    );
    assert_eq!(
        map(&fixture.root.0.join("external.csv"), "required-rollout")
            .unwrap_err()
            .code,
        "backup_invalid"
    );
    assert_eq!(
        map(
            &fixture.old_home.join("sessions/absent.jsonl"),
            "required-rollout"
        )
        .unwrap_err()
        .code,
        "backup_invalid"
    );
    assert_eq!(
        map(
            &fixture.old_home.join("jobs/not-created.csv"),
            "optional-owned-file"
        )
        .unwrap(),
        Some(node_path::from_native(
            &fixture.new_home.join("jobs/not-created.csv")
        ))
    );
    // SQLite may checkpoint during a failed attempt; logical rows and owned files remain intact.
    for (name, hash) in before
        .iter()
        .filter(|(name, _)| !name.to_utf8().unwrap().contains("state_5.sqlite"))
    {
        assert_eq!(&snapshot(&fixture.root.0.join("target")).await[name], hash);
    }
}
