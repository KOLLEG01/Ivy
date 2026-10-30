import { isRustRootInput } from '../build/rust-root-inputs.mjs';

export const scopes = [
  "data-collector",
  "dashboards",
  "hive",
  "sdk",
  "host",
  "agent",
  "chat",
  "secretary",
  "task-board",
  "phone",
  "automation",
  "ui",
  "tooling",
];
export function scope(file) {
  if (file.startsWith("tests/rust/host-")) return "host";
  if (/host-(?:codex-cases|build-input|compiler-tools|service-storage|native-(?:plan|coordinator|recovery))/.test(file)) return "host";
  if (/data-collector/.test(file)) return "data-collector";
  if (/dashboards/.test(file)) return "dashboards";
  if (
    /deployment-plan|build-(cache|inputs|rust-backend|native-dispatchers|native-payloads|chat-package)|test-(runner|selection|reporter)/.test(
      file,
    )
  )
    return "tooling";
  if (/phone[-_]|native-media-adapter/.test(file)) return "phone";
  if (/task-board/.test(file)) return "task-board";
  if (/chat/.test(file)) return "chat";
  if (/secretary/.test(file)) return "secretary";
  if (/automation/.test(file)) return "automation";
  if (/sdk/.test(file)) return "sdk";
  if (/native-|agent-|codex-|claude-|project-locations/.test(file)) return "agent";
  if (/web-e2e\/|ui-(content|live)/.test(file)) return "ui";
  if (
    /kernel|storage|backup|transports|connection-auth|hive[-_]|browser-push|contracts|builtin_schema|schema-compiler|concurrency|console-inspection|ui-publication/.test(
      file,
    )
  )
    return "hive";
  return "host";
}
export function kind(file) {
  if (/^tests\/rust\/phone_(?:media_codec|realtime(?:_native)?|micro_(?:transport|attachment|client_files|command_output|client)|desktop_(?:controls|actions|activation|process|application)|process_ownership|windows_controls|windows_(?:audio|route|render(?:_(?:pump|format|device|owner|status))?|capture(?:_(?:device|owner))?)|voice_input(?:_owner)?|voice_runtime|call_desktop(?:_faults)?|call_voice(?:_faults)?|audio_mute(?:_windows)?|inventory_windows|loopback_(?:probe|windows)|native_(?:audio|codec|inventory|composition(?:_entry)?|probe|voice_input|bootstrap|desktop|process|call_desktop(?:_faults|_entry)?))\.rs$/.test(file))
    return "native";
  if (file === 'tests/secretary-rust/document_process_cases.rs') return 'native';
  if (/tests\/web-e2e\//.test(file)) return "web";
  if (/phone-(audio|hotkey)\.test|phone-.*\.integration/.test(file))
    return "native";
  return "core";
}
export function affected(file) {
  if (isRustRootInput(file) || /^packages\/store\//.test(file)) return scopes.filter(s => s !== 'tooling');
  if (/^(services\/data-collector|docs\/examples\/data-collector|ui\/data-collector-ui)/.test(file)) return ["data-collector"];
  if (/^(services\/dashboards|ui\/dashboards-ui)/.test(file)) return ["dashboards"];
  // Semantic edges cover dynamic native/schema inputs which an import graph cannot see.
  if (
    /^docs\/examples\/services\/automation-example\/(?:deploy\.json|src\/(?:main|engine)\.ts)$/.test(
      file,
    )
  )
    return ["automation"];
  if (/^ui\/[^/]+\/deploy\.json$/.test(file)) return ["ui"];
  if (/^services\/[^/]+\/deploy\.json$/.test(file)) {
    const component = file.split("/")[1];
    return [
      {
        "host-executor": "host",
        "service-manager": "host",
        "agent-manager": "agent",
        "chat-bridge": "chat",
        secretary: "secretary",
        "phone-bridge": "phone",
        "automation-example": "automation",
      }[component] ?? component,
    ];
  }
  if (
    /^(?:services\/hive|packages\/(?:sdk|contracts))\/|^specs\/schemas\/(operation|hive|sdk|host)\./.test(
      file,
    )
  )
    return scopes.filter((x) => x !== "tooling");
  if (/^(package(-lock)?\.json|tsconfig.*\.json)$/.test(file))
    return scopes.filter((x) => x !== "tooling");
  if (/^services\/service-manager\//.test(file)) return ["host"];
  if (/^packages\/host-runtime\//.test(file))
    return ["host", "agent", "phone", "hive", "dashboards", "secretary"];
  if (
    /^(services\/phone-bridge|tests\/native-phone|specs\/schemas\/phone|tools\/(?:build|operations|package|testing)\/.*phone)/.test(
      file,
    )
  )
    return ["phone"];
  if (
    /^(services\/chat-bridge|specs\/schemas\/chat|tools\/contracts\/(?:generate-chat|synchronize-chat))/.test(
      file,
    )
  )
    return ["chat"];
  if (
    /^(services\/secretary|ui\/secretary-ui|specs\/schemas\/secretary|tools\/contracts\/generate-secretary)/.test(
      file,
    )
  )
    return ["secretary", "ui"];
  if (
    /^(services\/task-board|ui\/task-board-ui|specs\/schemas\/task-board|tools\/contracts\/generate-task-board)/.test(
      file,
    )
  )
    return ["task-board"];
  if (
    /^(services\/agent-manager|ui\/agent-ui|specs\/native|specs\/schemas\/agent|tools\/contracts\/(?:generate-native|generate-agent))/.test(
      file,
    )
  )
    return ["agent", "chat", "task-board", "sdk"];
  if (/^(packages\/ui|ui\/)/.test(file)) return ["ui"];
  if (
    /^(docs\/examples\/services\/automation-example|specs\/schemas\/automation|tools\/contracts\/generate-automation)/.test(
      file,
    )
  )
    return ["automation"];
  if (/^tools\/testing\//.test(file)) return ["tooling"];
  return [];
}
export const privateRepo = false;

// Explicit Cargo targets keep --files narrow and do not infer service ownership.
export const rustTests = {
  "tests/rust/data-collector-native-mcp.rs": {
    "package": "ivy-data-collector",
    "lib": true,
    "filter": "native_mcp::tests::",
    "testThreads": 1
  },
  "tests/rust/data-collector-http.rs": {
    "package": "ivy-data-collector",
    "test": "data-collector-http"
  },
  "tests/fixtures/task-board-document-json.rs": {
    package: "ivy-task-board", test: "task-board-host-runtime", filter: "document_json::",
  },
  "tests/fixtures/task-board-execution-completion.rs": {
    package: "ivy-task-board", lib: true, filter: "journal_store::execution_completion_tests::",
  },
  "tests/fixtures/task-board-cache.rs": {
    package: "ivy-task-board", lib: true,
    filter: "journal_store::persistent_reads::task_comments::memory_tests",
  },
  "tests/fixtures/task-board-journal-memory.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "journal_memory::",
    "testThreads": 1
  },
  "tests/secretary-rust/journal-memory.rs": {
    "package": "ivy-secretary",
    "test": "journal",
    "filter": "retention_memory::"
  },
  "tests/rust/hive_storage_thread.rs": { "package": "ivy-hive", "test": "hive-storage-thread" },
  "tests/rust/hive_query_cache.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "store::query::cache_tests::"
  },
  "tests/rust/hive_contract_catalog.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "contract_catalog::tests::",
    "testThreads": 1
  },
  "tests/rust/hive_routing_ownership.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "live_routing::ownership_tests::",
    "testThreads": 1
  },
  "tests/rust/hive_discovery_cache.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "discovery::cache_tests::"
  },
  "tests/rust/hive_discovery_memory.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "discovery::memory_tests::",
    "testThreads": 1
  },
  "tests/rust/hive_retention_graph.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "retention::graph_tests::",
    "testThreads": 1
  },
  "tests/rust/hive_retention_catalog.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "retention::catalog_tests::",
    "testThreads": 1
  },
  "tests/rust/hive_registry_catalog_memory.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "registry::catalog_memory_tests::",
    "testThreads": 1
  },
  "tests/rust/hive_catalog_reads.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "kernel::catalogs::read_tests::",
    "testThreads": 1
  },
  "tests/rust/hive_deployment_catalog.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "kernel::catalogs::deployments::tests::",
    "testThreads": 1
  },
  "tests/rust/hive_storage_payload_memory.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "storage::payload_tests::",
    "testThreads": 1
  },
  "tests/rust/hive_oauth_retention.rs": {
    "package": "ivy-hive",
    "lib": true,
    "filter": "worker_identity::retention_tests::",
    "testThreads": 1
  },
  "tests/rust/hive_registry_catalog.rs": {
    package: "ivy-hive",
    lib: true,
    filter: "registry_catalog::tests::",
    testThreads: 1,
  },
  "tests/rust/host-memory.rs": { package: "ivy-host-runtime", test: "host-memory" },
  "tests/rust/native-catalog-memory.rs": { package: "ivy-host-runtime", test: "native-catalog-memory", testThreads: 1 },
  "tests/rust/chat-files.rs": {
    "package": "ivy-chat-bridge",
    "lib": true,
    "filter": "files::tests"
  },
  "tests/rust/chat-wa-socket-cases.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-journal",
    "filter": "wa_socket::"
  },
  "tests/rust/phone_voice_notices.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_notices::tests",
    "testThreads": 1
  },
  "tests/rust/host-storage-retention-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-actions",
    "filter": "storage_retention_cases::",
    "testThreads": 1
  },
  "tests/rust/hive_storage_compaction.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-compaction",
    "testThreads": 1
  },
  "tests/rust/hive_ui_files.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::ui_files::ui_",
    "testThreads": 1
  },
  "tests/rust/hive_object_delete.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::object_delete::",
    "testThreads": 1
  },
  "tests/rust/hive_object_move.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::object_move::",
    "testThreads": 1
  },
  "tests/rust/hive_object_mutations.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::object_mutations::",
    "testThreads": 1
  },
  "tests/rust/hive_object_write.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::object_write::",
    "testThreads": 1
  },
  "tests/rust/hive_object_create.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::object_create::",
    "testThreads": 1
  },
  "tests/rust/hive_store.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::schema_authentication_",
    "testThreads": 1
  },
  "tests/rust/hive_store_history.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::history::",
    "testThreads": 1
  },
  "tests/rust/hive_worker_identity.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::identity::",
    "testThreads": 1
  },
  "tests/rust/hive_mcp_request.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::registry::mcp_request::",
    "testThreads": 1
  },
  "tests/rust/hive_package_catalog.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::package_catalog::",
    "testThreads": 1
  },
  "tests/rust/hive_package_kernel.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::package_kernel::",
    "testThreads": 1
  },
  "tests/rust/agent-native-rpc.rs": {
    "package": "ivy-host-runtime",
    "test": "native-rpc"
  },
  "tests/secretary-rust/assignment_tick_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "assignment_tick_cases::"
  },
  "tests/secretary-rust/assignment_windows_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "assignment_windows_cases::"
  },
  "tests/secretary-rust/notices_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "notices_cases::"
  },
  "tests/rust/contracts-json-utf16.rs": {
    "package": "ivy-contracts",
    "test": "json-utf16"
  },
  "tests/secretary-rust/assignment_delivery_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "assignment_delivery_cases::"
  },
  "tests/secretary-rust/browser_notice_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "browser_notice_cases::"
  },
  "tests/secretary-rust/notice_evidence_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "notice_evidence_cases::"
  },
  "tests/secretary-rust/notice_scan_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "notice_scan_cases::"
  },
  "tests/secretary-rust/error-outcome.rs": {
    "package": "ivy-secretary",
    "test": "error-outcome"
  },
  "tests/secretary-rust/triage_workflow_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "triage_workflow_cases::"
  },
  "tests/secretary-rust/triage_prompt_cases.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "filter": "triage_prompt_cases::"
  },
  "tests/secretary-rust/native-service.rs": {
    "package": "ivy-secretary",
    "test": "native-service"
  },
  "tests/rust/host-bootstrap-actions.rs": {
    "package": "ivy-host-runtime",
    "test": "host-actions",
    "filter": "bootstrap_cases::",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/host-portable-io-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-portable-dependencies",
    "filter": "portable_dependencies_cases::io_cases::",
    "testThreads": 1
  },
  "tests/rust/host-portable-http.rs": {
    "package": "ivy-host-runtime",
    "test": "host-portable-http"
  },
  "tests/rust/host-codex-job-path-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "codex_cases::job_path_cases::",
    "testThreads": 1
  },
  "tests/rust/host-bootstrap-updater-path-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "package_updater_cases::automatic_path_cases::",
    "testThreads": 1
  },
  "tests/rust/host-package-download-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "package_updater_cases::download_cases::",
    "testThreads": 1
  },
  "tests/rust/host-package-updater-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "package_updater_cases::",
    "testThreads": 1
  },
  "tests/rust/host-configuration-updater-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "configuration_updater_cases::",
    "testThreads": 1
  },
  "tests/rust/host-package-archive-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "package_archive_cases::",
    "testThreads": 1
  },
  "tests/rust/host-backup-files-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "backup_files_cases::native_",
    "testThreads": 1
  },
  "tests/rust/host-native-catalog-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-native-paths",
    "filter": "native_catalog_cases::",
    "testThreads": 1
  },
  "tests/rust/host-bootstrap-restart-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-actions",
    "filter": "bootstrap_restart_cases::",
    "testThreads": 1,
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-bootstrap-worker-path-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "bootstrap_maintenance_cases::worker_path_cases::",
    "testThreads": 1
  },
  "tests/rust/host-bootstrap-maintenance-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "bootstrap_maintenance_cases::bootstrap_maintenance_dropped_waiter_retains_transaction_locks_and_unrelated_owner",
    "testThreads": 1,
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-bootstrap-installation-cases.rs": {
    "package": "ivy-host-runtime",
    "lib": true,
    "filter": "bootstrap::installation::tests::",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/host-actions.rs": {
    "package": "ivy-host-runtime",
    "test": "host-actions",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1,
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-preparation-actions.rs": {
    "package": "ivy-host-runtime",
    "test": "host-actions",
    "filter": "preparation_cases::",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/host-backup-actions.rs": {
    "package": "ivy-host-runtime",
    "test": "host-actions",
    "filter": "backup_cases::",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/host-reset-actions.rs": {
    "package": "ivy-host-runtime",
    "test": "host-actions",
    "filter": "reset_cases::",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/host-reset-clear-cases.rs": {
    "package": "ivy-host-executor",
    "lib": true,
    "filter": "reset::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/store-sqlite-compat.rs": {
    "package": "ivy-store",
    "test": "store",
    "filter": "sqlite_compat::",
    "testThreads": 1
  },
  "tests/secretary-rust/document_process_cases.rs": {
    "package": "ivy-secretary",
    "lib": true,
    "filter": "document_process::tests::",
    "testThreads": 1
  },
  "tests/rust/store-coercion.rs": {
    "package": "ivy-store",
    "test": "store-coercion"
  },
  "tests/rust/store-mechanics.rs": {
    "package": "ivy-store",
    "test": "store",
    "filter": "storage_mechanics::",
    "testThreads": 1
  },
  "tests/rust/error-outcome.rs": {
    "package": "ivy-contracts",
    "test": "error-outcome"
  },
  "tests/rust/sdk-error-outcome.rs": {
    "package": "ivy-sdk",
    "test": "sdk-error-outcome"
  },
  "tests/rust/host-executor-service.rs": {
    "package": "ivy-host-executor",
    "test": "host-executor-service",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-backup-storage-cases.rs": {
    "package": "ivy-host-executor",
    "test": "host-executor-service",
    "filter": "backup_storage_cases::",
    "testThreads": 1,
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/sdk-fetch-response.rs": {
    "package": "ivy-sdk",
    "test": "sdk-fetch-response"
  },
  "tests/rust/sdk-fetch-credentials.rs": {
    "package": "ivy-sdk",
    "test": "sdk-fetch-credentials"
  },
  "tests/rust/sdk-collation.rs": {
    "package": "ivy-sdk",
    "test": "sdk-collation"
  },
  "tests/rust/contracts-native-urls.rs": {
    "package": "ivy-contracts",
    "test": "contracts",
    "filter": "native_urls::"
  },
  "tests/rust/contracts.rs": {
    "package": "ivy-contracts",
    "test": "contracts"
  },
  "tests/rust/hash.rs": {
    "package": "ivy-contracts",
    "test": "hash"
  },
  "tests/rust/json-depth.rs": {
    "package": "ivy-contracts",
    "test": "json-depth"
  },
  "tests/rust/schema.rs": {
    "package": "ivy-contracts",
    "test": "schema"
  },
  "tests/rust/schema-runtime.rs": {
    "package": "ivy-contracts",
    "test": "schema-runtime"
  },
  "tests/rust/canonical-bundle.rs": {
    "package": "ivy-contracts",
    "test": "canonical-bundle"
  },
  "tests/rust/domain-schemas.rs": {
    "package": "ivy-contracts",
    "test": "domain-schemas"
  },
  "tests/rust/native-contract.rs": {
    "package": "ivy-contracts",
    "test": "native-contract"
  },
  "tests/rust/checked-native-contract.rs": {
    "package": "ivy-contracts",
    "test": "checked-native-contract"
  },
  "tests/rust/native-catalog-path.rs": {
    "package": "ivy-contracts",
    "test": "native-catalog-path"
  },
  "tests/rust/cli-reset-ui.rs": {
    "package": "ivy-cli",
    "test": "cli-reset-ui"
  },
  "tests/rust/cli-arguments.rs": {
    "package": "ivy-cli",
    "test": "cli-arguments"
  },
  "tests/rust/host-cli.rs": {
    "package": "ivy-host-executor",
    "test": "host-cli",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/ui-route.rs": {
    "package": "ivy-contracts",
    "test": "ui-route"
  },
  "tests/rust/builtin_schema.rs": {
    "package": "ivy-contracts",
    "test": "builtin_schema"
  },
  "tests/rust/store.rs": {
    "package": "ivy-store",
    "test": "store"
  },
  "tests/rust/sdk.rs": {
    "package": "ivy-sdk",
    "test": "sdk"
  },
  "tests/rust/task-board-questions.rs": {
    "package": "ivy-sdk",
    "test": "task-board-questions"
  },
  "tests/rust/sdk-comment-reader.rs": {
    "package": "ivy-sdk",
    "test": "sdk-comment-reader"
  },
  "tests/rust/sdk-task-board-records.rs": {
    "package": "ivy-sdk",
    "test": "sdk-task-board-records"
  },
  "tests/rust/task-board-evidence.rs": {
    "package": "ivy-task-board",
    "test": "task-board-evidence"
  },
  "tests/fixtures/task-board-allocation-continuation.rs": {
    "package": "ivy-task-board",
    "lib": true,
    "filter": "allocation::continuation_tests::"
  },
  "tests/rust/task-board-operations.rs": {
    "package": "ivy-task-board",
    "test": "task-board-operations"
  },
  "tests/fixtures/task-board-large-task-writes.rs": {
    "package": "ivy-task-board",
    "test": "task-board-host-runtime",
    "filter": "large_task_writes::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-lifecycle.rs": {
    "package": "ivy-task-board",
    "test": "task-board-operations",
    "filter": "lifecycle::"
  },
  "tests/fixtures/task-board-engine-factory-lifecycle.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "service::factory::lifecycle::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-engine-factory-retention.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "service::factory::retention::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-engine-factory-delivery.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "service::factory::delivery::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-engine-factory-steering.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "service::factory::steering::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-current-history.rs": {
    "package": "ivy-task-board",
    "test": "task-board-operations",
    "filter": "current_history::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-retention.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "retention::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-evidence-support.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "original_journal_",
    "testThreads": 1
  },
  "tests/fixtures/task-board-host-service.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "host_service::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-native-advance.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "native_driver::advance::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-native-preparation.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "native_preparation::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-native-driver.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "native_driver::durable_preparation_matches_original",
    "testThreads": 1
  },
  "tests/fixtures/task-board-native-resume.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "native_driver::resume::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-journal-store.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "journal_store::inspection_owner_and_saved_primary_guards_use_the_journal_connection",
    "testThreads": 1
  },
  "tests/rust/task-board-blobs.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs"
  },
  "tests/rust/task-board-output.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output"
  },
  "tests/rust/task-board-host-runtime.rs": {
    "package": "ivy-task-board",
    "test": "task-board-host-runtime"
  },
  "tests/rust/task-board-maintenance.rs": {
    "package": "ivy-task-board",
    "test": "task-board-maintenance",
    "testThreads": 1
  },
  "tests/fixtures/task-board-generation.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "reconciler::generation::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-native-journal.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "native_journal::"
  },
  "tests/fixtures/task-board-attachment-recovery.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "publication::attachment_recovery::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-journal-client.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "journal_store::client::"
  },
  "tests/fixtures/task-board-local-pages.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "journal_store::pages::"
  },
  "tests/fixtures/task-board-workspaces.rs": {
    "package": "ivy-task-board",
    "test": "task-board-blobs",
    "filter": "workspaces::"
  },
  "tests/fixtures/task-board-comment-reads.rs": {
    "package": "ivy-task-board",
    "test": "task-board-host-runtime",
    "filter": "comment_reads::"
  },
  "tests/fixtures/task-board-record-upgrades.rs": {
    "package": "ivy-task-board",
    "test": "task-board-host-runtime",
    "filter": "record_upgrades::"
  },
  "tests/fixtures/task-board-categories.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "categories::"
  },
  "tests/fixtures/task-board-deliveries-journal.rs": {
    "package": "ivy-task-board",
    "test": "task-board-operations",
    "filter": "deliveries::journal::real_publication_delivery_identity_survives_lost_reply_reopen_and_owner_replacement",
    "testThreads": 1
  },
  "tests/fixtures/task-board-inspector.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "inspector::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-signals.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "signals::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-turn-evidence.rs": {
    "package": "ivy-task-board",
    "test": "task-board-evidence",
    "filter": "turn_evidence::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-native-publication.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "native_publication::",
    "testThreads": 1
  },
  "tests/fixtures/task-board-native-policies.rs": {
    "package": "ivy-task-board",
    "test": "task-board-output",
    "filter": "native_policies::"
  },
  "tests/fixtures/task-board-host-runtime.rs": {
    "package": "ivy-task-board",
    "test": "task-board-host-runtime",
    "filter": "native_contracts_keep_public_versions_and_local_workflow_routing"
  },
  "tests/rust/sdk-operation-ids.rs": {
    "package": "ivy-sdk",
    "test": "sdk-operation-ids"
  },
  "tests/rust/sdk-native.rs": {
    "package": "ivy-sdk",
    "test": "sdk-native"
  },
  "tests/rust/sdk-native-owner.rs": {
    "package": "ivy-sdk",
    "test": "sdk-native-owner"
  },
  "tests/rust/sdk-native-callback-errors.rs": {
    "package": "ivy-sdk",
    "test": "sdk-native-owner",
    "filter": "callback_error_bridge::",
    "testThreads": 1
  },
  "tests/rust/sdk-native-plan.rs": {
    "package": "ivy-sdk",
    "test": "sdk-native-plan"
  },
  "tests/rust/sdk-consumers.rs": {
    "package": "ivy-sdk",
    "test": "sdk-consumers"
  },
  "tests/rust/sdk-provider.rs": {
    "package": "ivy-sdk",
    "test": "sdk-provider"
  },
  "tests/rust/sdk-optional-rpc.rs": {
    "package": "ivy-sdk",
    "test": "sdk-optional-rpc"
  },
  "tests/rust/sdk-native-project.rs": {
    "package": "ivy-sdk",
    "test": "sdk-native-project"
  },
  "tests/rust/sdk-native-discovery.rs": {
    "package": "ivy-sdk",
    "test": "sdk-native-discovery"
  },
  "tests/rust/sdk-date-parse.rs": {
    "package": "ivy-sdk",
    "test": "sdk-date-parse"
  },
  "tests/secretary-rust/delivery.rs": {
    "package": "ivy-secretary",
    "test": "delivery",
    "prerequisites": [
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-job"
      }
    ]
  },
  "tests/rust/sdk-native-observations.rs": {
    "package": "ivy-sdk",
    "test": "sdk-native-observations"
  },
  "tests/rust/sdk-native-turn-contents.rs": {
    "package": "ivy-sdk",
    "test": "sdk-native-turn-contents"
  },
  "tests/rust/host-scheduler.rs": {
    "package": "ivy-host-runtime",
    "test": "host-scheduler"
  },
  "tests/rust/host-reset-stop-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "reset_stop_cases::",
    "testThreads": 1,
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-reset-release-cases.rs": {
    "package": "ivy-host-runtime",
    "lib": true,
    "filter": "runtime_reset::host::release_cases::",
    "testThreads": 1,
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-build-identity.rs": {
    "package": "ivy-host-runtime",
    "test": "host-build-identity"
  },
  "tests/rust/host-build-input-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-build-inputs"
  },
  "tests/rust/host-target-storage-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-native-paths",
    "filter": "target_storage_cases::",
    "testThreads": 1
  },
  "tests/rust/host-service-storage.rs": {
    "package": "ivy-host-runtime",
    "test": "host-service-storage"
  },
  "tests/rust/host-emitted-runtime-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-emitted-runtime",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-compiler-tools.rs": {
    "package": "ivy-host-runtime",
    "test": "host-compiler-tools",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-native-coordinator.rs": {
    "package": "ivy-host-executor",
    "test": "host-native-coordinator",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      },
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-health"
      }
    ]
  },
  "tests/rust/host-protected-config.rs": {
    "package": "ivy-host-runtime",
    "test": "host-protected-config"
  },
  "tests/rust/host-preparation-links.rs": {
    "package": "ivy-host-runtime",
    "test": "host-preparation-links"
  },
  "tests/rust/host-native-paths.rs": {
    "package": "ivy-host-runtime",
    "test": "host-native-paths"
  },
  "tests/rust/host-native-plan.rs": {
    "package": "ivy-host-runtime",
    "test": "host-native-plan",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-executor-entry.rs": {
    "package": "ivy-host-executor",
    "test": "host-executor-entry",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-regular-text-public-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-regular-text"
  },
  "tests/rust/host-regular-text-cases.rs": {
    "package": "ivy-host-runtime",
    "lib": true,
    "filter": "filesystem::regular_text::tests::"
  },
  "tests/rust/host-filesystem-remove-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-filesystem-remove"
  },
  "tests/rust/hive_online_backup.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::online_backup::",
    "testThreads": 1
  },
  "tests/rust/hive_reset_storage.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "reset_storage::",
    "testThreads": 1
  },
  "tests/rust/hive_storage_read.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "storage_read::",
    "testThreads": 1
  },
  "tests/rust/hive_storage_inspector.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "storage_read::inspector::native_",
    "testThreads": 1
  },
  "tests/rust/hive_filesystem.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::ui_files::filesystem::native_",
    "testThreads": 1
  },
  "tests/fixtures/hive-native-static.rs": {
    "package": "ivy-hive",
    "test": "hive-http",
    "filter": "native_static::native_browser_assets_and_authenticated_console_files",
    "testThreads": 1
  },
  "tests/rust/hive_packages.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::packages::",
    "testThreads": 1
  },
  "tests/rust/hive_ui_collection_causes.rs": {
    "package": "ivy-hive",
    "test": "hive-ui-collection-causes"
  },
  "tests/rust/hive_admission.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-thread",
    "filter": "admission::"
  },
  "tests/rust/hive_browser_push.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::kernel::browser_push::",
    "testThreads": 1
  },
  "tests/rust/hive_ledger.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::ledger::",
    "testThreads": 1
  },
  "tests/rust/hive_contracts.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::contracts::",
    "testThreads": 1
  },
  "tests/rust/hive_events.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::events::",
    "testThreads": 1
  },
  "tests/rust/hive_retention.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::retention::",
    "testThreads": 1
  },
  "tests/rust/hive_registry.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::registry::registry_",
    "testThreads": 1
  },
  "tests/rust/hive_search.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::search::search_",
    "testThreads": 1
  },
  "tests/rust/hive_kernel.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::kernel::kernel_",
    "testThreads": 1
  },
  "tests/rust/hive_kernel_settings.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::kernel::settings::",
    "testThreads": 1
  },
  "tests/rust/hive_kernel_content.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::kernel::content::",
    "testThreads": 1
  },
  "tests/rust/hive_kernel_domains.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::kernel::domains::",
    "testThreads": 1
  },
  "tests/rust/hive_wiki.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::wiki::wiki_",
    "testThreads": 1
  },
  "tests/rust/hive_discovery.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::registry::discovery::",
    "testThreads": 1
  },
  "tests/rust/hive_configurations.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::registry::configurations::",
    "testThreads": 1
  },
  "tests/rust/hive_uis.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::registry::uis::",
    "testThreads": 1
  },
  "tests/rust/hive_inventory.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::registry::inventory::",
    "testThreads": 1
  },
  "tests/rust/hive_mcp.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::registry::mcp::",
    "testThreads": 1
  },
  "tests/rust/hive_worker_kernel.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "::worker_kernel::kernel_worker_",
    "testThreads": 1
  },
  "tests/rust/hive_worker_actions.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::kernel::worker_actions::",
    "testThreads": 1
  },
  "tests/rust/hive_kernel_startup.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::kernel::startup::native_",
    "testThreads": 1
  },
  "tests/rust/hive_browser_push_sender.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "::push_sender::browser_push_sender_",
    "testThreads": 1
  },
  "tests/rust/hive_http_auth.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "::authentication::http_auth_",
    "testThreads": 1
  },
  "tests/rust/hive_browser_session.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "::browser_session::browser_session_",
    "testThreads": 1
  },
  "tests/rust/hive_auth_pages.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "::auth_pages::",
    "testThreads": 1
  },
  "tests/rust/hive_oauth.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "::oauth::oauth_native_",
    "testThreads": 1
  },
  "tests/rust/hive_oauth_http.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "::oauth_http::oauth_http_",
    "testThreads": 1
  },
  "tests/rust/hive_browser_push_http.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "::http::push_http_",
    "testThreads": 1
  },
  "tests/rust/hive_query.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker",
    "filter": "store::query::",
    "testThreads": 1
  },
  "tests/rust/hive_server_peers.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_peers::",
    "testThreads": 1
  },
  "tests/rust/hive_server_direct.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_peers::direct::",
    "testThreads": 1
  },
  "tests/rust/hive_server_composition.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_peers::composition::",
    "testThreads": 1
  },
  "tests/rust/hive_server_constructor.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "native_constructor",
    "testThreads": 1
  },
  "tests/rust/hive_server_constructor_cases.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_constructor::constructor_cases::",
    "testThreads": 1
  },
  "tests/rust/hive_server_options.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_options::",
    "testThreads": 1
  },
  "tests/rust/hive_core_calls.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "core_calls::",
    "testThreads": 1
  },
  "tests/rust/hive_mcp_protocol.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "mcp_protocol::",
    "testThreads": 1
  },
  "tests/rust/hive_mcp_inbound.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "mcp_inbound::",
    "testThreads": 1
  },
  "tests/rust/hive_mcp_parameters.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "mcp_parameters::",
    "testThreads": 1
  },
  "tests/rust/hive_mcp_dispatch.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "mcp_dispatch::",
    "testThreads": 1
  },
  "tests/rust/hive_mcp_subscription.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "mcp_subscription::",
    "testThreads": 1
  },
  "tests/rust/hive_mcp_owner.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_constructor::mcp_owner::",
    "testThreads": 1
  },
  "tests/rust/host-storage-space-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-storage-space"
  },
  "tests/rust/host-paths.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths"
  },
  "tests/rust/host-codex-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "codex_cases::",
    "testThreads": 1
  },
  "tests/rust/host-layout-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "layout_cases::",
    "testThreads": 1
  },
  "tests/rust/host-startup-cases.rs": {
    "package": "ivy-host-runtime",
    "test": "host-paths",
    "filter": "host_startup_cases::",
    "testThreads": 1
  },
  "tests/rust/host-windows-arguments.rs": {
    "package": "ivy-host-runtime",
    "test": "host-windows-arguments"
  },
  "tests/rust/host-process-output.rs": {
    "package": "ivy-host-runtime",
    "test": "host-process-output"
  },
  "tests/rust/host-process-output-streaming.rs": {
    "package": "ivy-host-runtime",
    "test": "host-process-output-streaming"
  },
  "tests/rust/host-process.rs": {
    "package": "ivy-host-runtime",
    "test": "host-process"
  },
  "tests/rust/host-exec-file-deadline.rs": {
    "package": "ivy-host-runtime",
    "lib": true,
    "filter": "exec_file::deadline_cases::",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-job",
        "platform": "win32"
      }
    ]
  },
  "tests/rust/host-package-requirements.rs": {
    "package": "ivy-host-runtime",
    "test": "host-package-requirements"
  },
  "tests/rust/host-defaults.rs": {
    "package": "ivy-host-runtime",
    "test": "host-defaults"
  },
  "tests/rust/host-logs.rs": {
    "package": "ivy-host-runtime",
    "test": "host-logs"
  },
  "tests/rust/agent-claude.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-claude"
  },
  "tests/rust/agent-claude-adapter.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-claude-adapter"
  },
  "tests/rust/agent-native-backup-paths.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-native-backup-paths"
  },
  "tests/rust/agent-skills-files.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-skills-files"
  },
  "tests/rust/agent-offline-import.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-offline-import"
  },
  "tests/rust/agent-runtime-reset.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-runtime-entry",
    "filter": "runtime_reset_cases::",
    "testThreads": 1
  },
  "tests/rust/agent-runtime-entry.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-runtime-entry",
    "prerequisites": [
      {
        "package": "ivy-host-runtime",
        "bin": "ivy-health"
      },
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-job"
      },
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-host-job"
      }
    ]
  },
  "tests/rust/agent-native-owner.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-runtime-entry",
    "filter": "native_owner_cases::",
    "testThreads": 1,
    "prerequisites": [
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-job"
      }
    ]
  },
  "tests/rust/agent-claude-events.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-claude-events"
  },
  "tests/rust/agent-skills-locale.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-skills-files"
  },
  "tests/rust/agent-state.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-state"
  },
  "tests/rust/agent-journal.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-journal",
    "testThreads": 1
  },
  "tests/rust/agent-catalog-state.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-catalog-state",
    "testThreads": 1
  },
  "tests/rust/agent-notifications.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-notifications"
  },
  "tests/rust/agent-interactions.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-interactions",
    "prerequisites": [
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-job"
      }
    ]
  },
  "tests/rust/agent-instructions.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions"
  },
  "tests/fixtures/agent-instructions-cases.json.gz": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "inheritance_local_bytes_and_version_identities_match_original"
  },
  "tests/rust/agent-instructions-files.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "instruction_file_cases::",
    "testThreads": 1
  },
  "tests/rust/agent-environment.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "environment_cases::",
    "testThreads": 1
  },
  "tests/rust/agent-instructions-source.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "instruction_source_cases::",
    "testThreads": 1
  },
  "tests/rust/agent-environment-text.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "environment_text_cases::"
  },
  "tests/rust/agent-environment-conflicts.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "environment_conflict_cases::"
  },
  "tests/rust/agent-mcp-output.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "mcp_output_cases::"
  },
  "tests/rust/agent-mcp-files.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "mcp_file_cases::"
  },
  "tests/rust/agent-mcp-render.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "mcp_render_cases::"
  },
  "tests/rust/agent-staged-files.rs": {
    "test": "agent-interactions",
    "package": "ivy-agent-manager",
    "filter": "service_kernel::staged_files::",
    "testThreads": 1
  },
  "tests/rust/agent-skills-output.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-instructions",
    "filter": "skills_output_cases::"
  },
  "tests/rust/agent-native-restore.rs": {
    "package": "ivy-agent-manager",
    "test": "agent-native-restore"
  },
  "tests/rust/service-manager-policy.rs": {
    "package": "ivy-service-manager",
    "test": "service-manager-policy"
  },
  "tests/rust/service-manager-data.rs": {
    "package": "ivy-service-manager",
    "test": "service-manager-data"
  },
  "tests/rust/service-manager-shutdown.rs": {
    "package": "ivy-service-manager",
    "test": "service-manager-shutdown"
  },
  "tests/rust/service-manager-stop.rs": {
    "package": "ivy-service-manager",
    "test": "service-manager-stop",
    "prerequisites": [
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-job"
      },
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-health"
      }
    ]
  },
  "tests/rust/data-collector-store.rs": {
    "package": "ivy-data-collector",
    "test": "data-collector-store"
  },
  "tests/rust/data-collector-policy.rs": {
    "package": "ivy-data-collector",
    "test": "data-collector-policy"
  },
  "tests/rust/data-collector-worker.rs": {
    "package": "ivy-data-collector",
    "test": "data-collector-worker"
  },
  "tests/rust/data-collector-publication.rs": {
    "package": "ivy-data-collector",
    "test": "data-collector-publication"
  },
  "tests/rust/dashboards-raster.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-raster"
  },
  "tests/rust/dashboards-browser-path.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-browser-path"
  },
  "tests/rust/dashboards-png.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-png"
  },
  "tests/rust/dashboards-document.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-document"
  },
  "tests/rust/dashboards-renderer.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-renderer"
  },
  "tests/rust/hive_push_native.rs": {
    "package": "ivy-hive",
    "test": "hive-push-native"
  },
  "tests/rust/hive_push_profile.rs": {
    "package": "ivy-hive",
    "test": "hive-push-profile"
  },
  "tests/rust/hive_package_ui.rs": {
    "package": "ivy-hive",
    "test": "hive-package-ui"
  },
  "tests/rust/hive_http.rs": {
    "package": "ivy-hive",
    "test": "hive-http",
    "filter": "original_"
  },
  "tests/rust/hive_dashboard_image.rs": {
    "package": "ivy-hive",
    "test": "hive-dashboard-image"
  },
  "tests/rust/hive_network.rs": {
    "package": "ivy-hive",
    "test": "hive-network"
  },
  "tests/rust/hive_network_cases.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "network_cases::",
    "testThreads": 1
  },
  "tests/rust/hive_server_lifecycle.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_lifecycle::",
    "testThreads": 1
  },
  "tests/rust/hive_server_authentication.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_constructor::authentication::",
    "testThreads": 1
  },
  "tests/rust/hive_server_http.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_constructor::http::",
    "testThreads": 1
  },
  "tests/rust/hive_server_http_rpc.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_constructor::http::rpc::",
    "testThreads": 1
  },
  "tests/rust/hive_server_mcp.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_constructor::http::mcp::",
    "testThreads": 1
  },
  "tests/rust/hive_ws_socket_io.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "ws_socket_io::",
    "testThreads": 1
  },
  "tests/rust/hive_server_rpc.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "server_rpc::",
    "testThreads": 1
  },
  "tests/rust/hive_dispatch_gate.rs": {
    "package": "ivy-hive",
    "test": "hive-network",
    "filter": "dispatch_gate::",
    "testThreads": 1
  },
  "tests/rust/hive_offline_backup.rs": {
    "package": "ivy-hive",
    "test": "hive-offline-backup"
  },
  "tests/rust/hive_storage_worker.rs": {
    "package": "ivy-hive",
    "test": "hive-storage-worker"
  },
  "tests/secretary-rust/document.rs": {
    "package": "ivy-secretary",
    "test": "document"
  },
  "tests/secretary-rust/journal.rs": {
    "package": "ivy-secretary",
    "test": "journal"
  },
  "tests/secretary-rust/schedule-policy.rs": {
    "package": "ivy-secretary",
    "test": "schedule-policy"
  },
  "tests/secretary-rust/media-input.rs": {
    "package": "ivy-secretary",
    "test": "media-input"
  },
  "tests/rust/dashboards-runtime.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-runtime"
  },
  "tests/rust/dashboards-sources.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-sources"
  },
  "tests/rust/dashboards-validation.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-validation"
  },
  "tests/rust/dashboards-store.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-store"
  },
  "tests/rust/dashboards-handlers.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-handlers"
  },
  "tests/rust/dashboards-browser.rs": {
    "package": "ivy-dashboards",
    "test": "dashboards-browser"
  },
  "tests/rust/phone_micro_wire.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "micro::tests::",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_native_owner.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "native_owner::",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_audio_sender.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "audio_sender::",
    "testThreads": 1
  },
  "tests/rust/phone_call_audio.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::audio_",
    "testThreads": 1
  },
  "tests/rust/phone_call_features.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_features::",
    "testThreads": 1
  },
  "tests/rust/phone_native_dial.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::dial::",
    "testThreads": 1
  },
  "tests/rust/phone_native_answer.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::answer::",
    "testThreads": 1
  },
  "tests/rust/phone_native_incoming.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::incoming::",
    "testThreads": 1
  },
  "tests/rust/phone_native_feedback.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::feedback::",
    "testThreads": 1
  },
  "tests/rust/phone_native_screening.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::screening::",
    "testThreads": 1
  },
  "tests/rust/phone_native_requests.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::requests::",
    "testThreads": 1
  },
  "tests/rust/phone_native_info.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::info::",
    "testThreads": 1
  },
  "tests/rust/phone_native_upgrade.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::upgrade::",
    "testThreads": 1
  },
  "tests/rust/phone_session.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::session_public_",
    "testThreads": 1
  },
  "tests/rust/phone_media_diagnostics.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::media_diagnostics::",
    "testThreads": 1
  },
  "tests/rust/phone_media_format.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "media_format::",
    "testThreads": 1
  },
  "tests/rust/phone_pcm_codec.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "pcm_codec::pcm_native_",
    "testThreads": 1
  },
  "tests/rust/phone_resampler.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "resampler::",
    "testThreads": 1
  },
  "tests/rust/phone_sip_udp.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "sip::udp::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_rtcp.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "rtcp::",
    "testThreads": 1
  },
  "tests/rust/phone_rtcp_owner.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "rtcp_owner::",
    "testThreads": 1
  },
  "tests/rust/phone_rtcp_session.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "rtcp_session::",
    "testThreads": 1
  },
  "tests/rust/phone_rtcp_state.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "rtcp_state::",
    "testThreads": 1
  },
  "tests/rust/phone_rtcp_runtime_private.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::receive_owner::report_tests::",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_rtp_transport.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "rtp_transport::",
    "testThreads": 1
  },
  "tests/rust/phone_sdp.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "sdp::",
    "testThreads": 1
  },
  "tests/rust/phone_sip_receiver.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "sip_receiver::",
    "testThreads": 1
  },
  "tests/rust/phone_realtime_route.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::realtime::tests::route_tests::route_contracts::",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_sender_runtime_private.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::sender::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_native_construction_prefix.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::construction_prefix::",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_native_lifecycle_boundaries.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::lifecycle_boundaries::",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_native_setup_order.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::setup_order::",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_native_status.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::status::",
    "testThreads": 1
  },
  "tests/rust/phone_native_sip_commands.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::native_lifecycle_dto_and_error_order_",
    "testThreads": 1
  },
  "tests/rust/phone_native_sip.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::admitted_factory_uses_sdk_identity_",
    "testThreads": 1
  },
  "tests/rust/phone_native_config.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "native_config::",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_native_registration.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::registration::",
    "testThreads": 1
  },
  "tests/rust/phone_audio_setup.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "service_bridge::audio::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_executables.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_executables",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64"
    ],
    "testThreads": 1,
    "prerequisites": [
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-job"
      }
    ]
  },
  "tests/rust/phone_service_reference.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_service_reference",
    "testThreads": 1
  },
  "tests/rust/phone_service_flow.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "service_flow::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_call_logs.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "call_logs::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_call_commands.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "call_commands::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_service_settings.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "service_settings::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_service_registry.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "service_registry::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_service_process.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "service_process::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_admission.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "admission::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_service_native_client.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "native_client::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_owner.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_rpc::owner::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_work.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_work::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_journal.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "journal::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_preparation.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_rpc::preparation::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_process.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_rpc::process::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_rpc.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "voice_rpc::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_schemas.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_rpc::schema_tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_control.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_rpc::control::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_speakers.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_rpc::speakers::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_voice_models.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "voice_rpc::models::tests::",
    "testThreads": 1
  },
  "tests/rust/phone_media_codec.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "media_codec::media_codec_",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_realtime.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::realtime::tests::protocol_tests::",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_realtime_native.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::realtime::tests::route_tests::native_tests::",
    "platforms": [
      "win32",
      "linux"
    ],
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_route.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_route::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_render_owner.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_render::worker::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_render_status.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_render::status::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_render_device.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_render::device::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_render_format.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_render::format::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_render_pump.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_render::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_render.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_audio::render::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_capture_device.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_capture::device::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_capture_owner.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_capture::worker::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_capture.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_capture::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_windows_audio.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::windows_audio::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_codec.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::codec::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_inventory.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::inventory::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_inventory_windows.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "native_inventory::windows::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_composition.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::composition::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_composition_entry.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::composition::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_audio_mute.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "audio_mute::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_audio_mute_windows.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "media::mute::windows::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_loopback_probe.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "loopback_probe::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_probe.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::probe::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_loopback_windows.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "loopback_probe::windows::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_audio.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::audio::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_voice_input.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "native_sip_commands::voice_input_tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_micro_client.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "micro::client::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1,
    "prerequisites": [
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-job"
      }
    ]
  },
  "tests/rust/phone_micro_command_output.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "micro::command_output::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_micro_client_files.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "micro::client_files::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_micro_attachment.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "micro::attachment::tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_micro_transport.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "micro_transport::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_call_desktop_entry.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::desktop::exception_policy::entry::actual_call_owner_keeps_entry_lease_until_process_exit",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_call_desktop_faults.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::desktop::exception_policy::final_audio_failure_fences_actual_native_release_and_shutdown",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_call_desktop.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "call_audio::session::native_operations::commands::desktop::native_desktop_dto_receipts_and_retirement",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_call_desktop_faults.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::call_session::tests::exception_policy::final_audio_failure_retains_call_owner_and_blocks_cleanup_retry",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_call_desktop.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::call_session::tests::original_call_admission_generations_retained_voice_and_drain",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_voice_runtime.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::voice_runtime::tests::composed_owner_input_focus_dispatch_and_cleanup_preserve_boundaries",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_call_voice_faults.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::call_voice::tests::exception_policy::final_audio_failure_preserves_completion_and_fences_cleanup",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_call_voice.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::call_voice::tests::retained_voice_capture_authority_transitions_and_cleanup",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_process.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "native_desktop::tests::process_commands::ownership_validation_projection_and_uncancelled_reads_agree",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_process_ownership.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "native_desktop::tests::process_ownership::scripted_ancestry_and_capture_correlation",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_desktop.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "native_desktop::tests::validation_replay_and_shutdown_preserve_original_application_work",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_desktop_application.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::windows_application::tests::candidate_admission_order_and_cleanup_preserve_original_observation",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_desktop_process.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::windows_owner::tests::process_identity_window_and_focus_callbacks_preserve_original_checks",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_native_bootstrap.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "native_entry::bootstrap_tests::",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_desktop_controls.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::tests::original_control_boundaries_classification_and_thread_bound_session_agree",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_windows_controls.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::windows::tests::sdk_com_cache_refresh_invoke_and_lifetime_match_original",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_desktop_actions.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::windows_input::tests::original_launch_input_focus_and_invoke_policy_agree",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_desktop_activation.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::windows_activation::tests::original_activation_hresult_pid_and_cleanup_match_sdk_boundaries",
    "platform": "win32",
    "architectures": [
      "x64",
      "arm64"
    ],
    "testThreads": 1
  },
  "tests/rust/phone_voice_input.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::voice_input::tests::original_voice_settings_gesture_authority_and_fallback_agree",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_voice_input_owner.rs": {
    "package": "ivy-phone-bridge",
    "lib": true,
    "filter": "desktop::input_owner::tests::original_selection_retry_reentry_and_disposal_owner_agree",
    "platform": "win32",
    "testThreads": 1
  },
  "tests/rust/phone_sip_tls.rs": {
    "package": "ivy-phone-bridge",
    "lib": true
  },
  "tests/rust/phone_media.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_media"
  },
  "tests/rust/phone_diagnostics.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_diagnostics"
  },
  "tests/rust/phone_native_rpc.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "rpc_contracts::",
    "testThreads": 1
  },
  "tests/rust/phone_native_dispatch.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "native_dispatch::",
    "testThreads": 1
  },
  "tests/rust/phone_native_pipe.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "native_pipe::",
    "testThreads": 1
  },
  "tests/rust/phone_register.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "register::",
    "testThreads": 1
  },
  "tests/rust/phone_registration.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "registration_contracts::",
    "testThreads": 1
  },
  "tests/rust/phone_sip.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "sip::",
    "testThreads": 1
  },
  "tests/rust/phone_sip_host.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "sip_host::",
    "testThreads": 1
  },
  "tests/rust/phone_sip_stack.rs": {
    "package": "ivy-phone-bridge",
    "test": "phone_native_rpc",
    "filter": "sip_stack::",
    "testThreads": 1
  },
  "tests/rust/chat-notices.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-notices"
  },
  "tests/rust/chat-result-text.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-result-text"
  },
  "tests/rust/chat-collection-reads.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-collection-reads"
  },
  "tests/rust/chat-evidence-bytes.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-evidence-bytes"
  },
  "tests/rust/chat-native-driver.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-native-driver"
  },
  "tests/rust/chat-service-active-cases.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-native-driver",
    "filter": "runtime::active::"
  },
  "tests/rust/chat-registry.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-registry"
  },
  "tests/rust/chat-input-media.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-input-media"
  },
  "tests/rust/chat-admission.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-admission"
  },
  "tests/rust/chat-queue.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-queue"
  },
  "tests/rust/chat-main-context.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-main-context"
  },
  "tests/rust/chat-outbox.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-outbox"
  },
  "tests/rust/chat-queries.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-queries"
  },
  "tests/rust/chat-storage.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-storage",
    "prerequisites": [
      {
        "platform": "win32",
        "package": "ivy-host-runtime",
        "bin": "ivy-job"
      }
    ]
  },
  "tests/rust/chat-audio.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-audio"
  },
  "tests/rust/chat-output.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-output"
  },
  "tests/rust/chat-identity.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-identity"
  },
  "tests/rust/chat-journal.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-journal"
  },
  "tests/rust/chat-diagnostics.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-diagnostics"
  },
  "tests/rust/chat-sender.rs": {
    "package": "ivy-chat-bridge",
    "test": "chat-sender"
  },
  "tests/rust/agent-service-kernel.rs": {
    "test": "agent-interactions",
    "package": "ivy-agent-manager",
    "filter": "service_kernel::",
    "testThreads": 1
  },
  "tests/rust/agent-inventory-manager.rs": {
    "test": "agent-interactions",
    "package": "ivy-agent-manager",
    "filter": "inventory_manager::",
    "testThreads": 1
  },
  "tests/rust/agent-environment-defaults.rs": {
    "test": "agent-interactions",
    "package": "ivy-agent-manager",
    "filter": "environment_defaults::",
    "testThreads": 1
  },
  "tests/rust/host-artifact-inventory-cases.rs": {
    "test": "host-paths",
    "package": "ivy-host-runtime",
    "filter": "artifact_inventory_cases::",
    "testThreads": 1
  },
  "tests/rust/chat-desktop-host-cases.rs": {
    "test": "chat-journal",
    "testThreads": 1,
    "package": "ivy-chat-bridge",
    "filter": "desktop_host::"
  }
};

// Only reviewed, isolated tests may reuse a successful result. Integration tests always run.
export const cacheable = (file) =>
  ["tests/build-cache.test.mjs"].includes(file);
