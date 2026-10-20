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
  if (/data-collector/.test(file)) return "data-collector";
  if (/dashboards/.test(file)) return "dashboards";
  if (
    /deployment-plan|build-(cache|inputs)|test-(runner|selection|reporter)/.test(
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
  if (/tests\/web-e2e\//.test(file)) return "web";
  if (/phone-(audio|hotkey)\.test|phone-.*\.integration/.test(file))
    return "native";
  return "core";
}
export function affected(file) {
  if (/^(?:Cargo\.(?:toml|lock)|rust-toolchain\.toml|packages\/store\/)/.test(file)) return scopes.filter(s => s !== 'tooling');
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
  if (/^packages\/host-runtime\//.test(file))
    return ["host", "agent", "phone", "hive"];
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
  'tests/rust/contracts.rs': { package: 'ivy-contracts', test: 'contracts' },
  'tests/rust/hash.rs': { package: 'ivy-contracts', test: 'hash' },
  'tests/rust/json-depth.rs': { package: 'ivy-contracts', test: 'json-depth' },
  'tests/rust/schema.rs': { package: 'ivy-contracts', test: 'schema' },
  'tests/rust/schema-runtime.rs': { package: 'ivy-contracts', test: 'schema-runtime' },
  'tests/rust/canonical-bundle.rs': { package: 'ivy-contracts', test: 'canonical-bundle' },
  'tests/rust/domain-schemas.rs': { package: 'ivy-contracts', test: 'domain-schemas' },
  'tests/rust/native-contract.rs': { package: 'ivy-contracts', test: 'native-contract' },
  'tests/rust/checked-native-contract.rs': { package: 'ivy-contracts', test: 'checked-native-contract' },
  'tests/rust/native-catalog-path.rs': { package: 'ivy-contracts', test: 'native-catalog-path' },
  'tests/rust/json-coercion.rs': { package: 'ivy-contracts', test: 'json-coercion' },
  'tests/rust/cli-publish-ui.rs': { package: 'ivy-cli', test: 'cli-publish-ui' },
  'tests/rust/cli-arguments.rs': { package: 'ivy-cli', test: 'cli-arguments' },
  'tests/rust/number-string.rs': { package: 'ivy-contracts', test: 'number-string' },
  'tests/rust/ui-route.rs': { package: 'ivy-contracts', test: 'ui-route' },
  'tests/rust/builtin_schema.rs': { package: 'ivy-contracts', test: 'builtin_schema' },
  'tests/rust/store.rs': { package: 'ivy-store', test: 'store' },
  'tests/rust/sdk.rs': { package: 'ivy-sdk', test: 'sdk' },
  'tests/rust/sdk-operation-ids.rs': { package: 'ivy-sdk', test: 'sdk-operation-ids' },
  'tests/rust/sdk-native.rs': { package: 'ivy-sdk', test: 'sdk-native' },
  'tests/rust/sdk-consumers.rs': { package: 'ivy-sdk', test: 'sdk-consumers' },
  'tests/rust/sdk-provider.rs': { package: 'ivy-sdk', test: 'sdk-provider' },
  'tests/rust/sdk-native-project.rs': { package: 'ivy-sdk', test: 'sdk-native-project' },
  'tests/rust/sdk-native-discovery.rs': { package: 'ivy-sdk', test: 'sdk-native-discovery' },
  'tests/rust/sdk-date-parse.rs': { package: 'ivy-sdk', test: 'sdk-date-parse' },
  'tests/secretary-rust/delivery.rs': { package: 'ivy-secretary', test: 'delivery' },
  'tests/rust/sdk-native-observations.rs': { package: 'ivy-sdk', test: 'sdk-native-observations' },
  'tests/rust/host-scheduler.rs': { package: 'ivy-host-runtime', test: 'host-scheduler' },
  'tests/rust/host-build-identity.rs': { package: 'ivy-host-runtime', test: 'host-build-identity' },
  'tests/rust/host-paths.rs': { package: 'ivy-host-runtime', test: 'host-paths' },
  'tests/rust/host-windows-arguments.rs': { package: 'ivy-host-runtime', test: 'host-windows-arguments' },
  'tests/rust/host-process-output.rs': { package: 'ivy-host-runtime', test: 'host-process-output' },
  'tests/rust/host-process-output-node.rs': { package: 'ivy-host-runtime', test: 'host-process-output-node' },
  'tests/rust/host-process.rs': { package: 'ivy-host-runtime', test: 'host-process' },
  'tests/rust/host-package-requirements.rs': { package: 'ivy-host-runtime', test: 'host-package-requirements' },
  'tests/rust/host-defaults.rs': { package: 'ivy-host-runtime', test: 'host-defaults' },
  'tests/rust/host-logs.rs': { package: 'ivy-host-runtime', test: 'host-logs' },
  'tests/rust/agent-state.rs': { package: 'ivy-agent-manager', test: 'agent-state' },
  'tests/rust/agent-notifications.rs': { package: 'ivy-agent-manager', test: 'agent-notifications' },
  'tests/rust/agent-interactions.rs': { package: 'ivy-agent-manager', test: 'agent-interactions' },
  'tests/rust/agent-instructions.rs': { package: 'ivy-agent-manager', test: 'agent-instructions' },
  'tests/rust/service-manager-policy.rs': { package: 'ivy-service-manager', test: 'service-manager-policy' },
  'tests/rust/service-manager-data.rs': { package: 'ivy-service-manager', test: 'service-manager-data' },
  'tests/rust/service-manager-shutdown.rs': { package: 'ivy-service-manager', test: 'service-manager-shutdown' },
  'tests/rust/service-manager-stop.rs': {
    package: 'ivy-service-manager', test: 'service-manager-stop',
    prerequisites: [{ platform: 'win32', package: 'ivy-host-runtime', bin: 'ivy-job' }],
  },
  'tests/rust/chat-storage.rs': { package: 'ivy-chat-bridge', test: 'chat-storage' },
  'tests/rust/chat-audio.rs': { package: 'ivy-chat-bridge', test: 'chat-audio' },
  'tests/rust/chat-output.rs': { package: 'ivy-chat-bridge', test: 'chat-output' },
  'tests/rust/chat-identity.rs': { package: 'ivy-chat-bridge', test: 'chat-identity' },
  'tests/rust/chat-journal.rs': { package: 'ivy-chat-bridge', test: 'chat-journal' },
  'tests/rust/chat-diagnostics.rs': { package: 'ivy-chat-bridge', test: 'chat-diagnostics' },
  'tests/rust/chat-sender.rs': { package: 'ivy-chat-bridge', test: 'chat-sender' },
  'tests/rust/data-collector-store.rs': { package: 'ivy-data-collector', test: 'data-collector-store' },
  'tests/rust/data-collector-policy.rs': { package: 'ivy-data-collector', test: 'data-collector-policy' },
  'tests/rust/data-collector-worker.rs': { package: 'ivy-data-collector', test: 'data-collector-worker' },
  'tests/rust/data-collector-publication.rs': { package: 'ivy-data-collector', test: 'data-collector-publication' },
  'tests/rust/dashboards-raster.rs': { package: 'ivy-dashboards', test: 'dashboards-raster' },
  'tests/rust/dashboards-browser-path.rs': { package: 'ivy-dashboards', test: 'dashboards-browser-path' },
  'tests/rust/dashboards-png.rs': { package: 'ivy-dashboards', test: 'dashboards-png' },
  'tests/rust/dashboards-document.rs': { package: 'ivy-dashboards', test: 'dashboards-document' },
  'tests/rust/dashboards-renderer.rs': { package: 'ivy-dashboards', test: 'dashboards-renderer' },
  'tests/rust/hive_push_native.rs': { package: 'ivy-hive', test: 'hive-push-native' },
  'tests/rust/hive_storage_worker.rs': { package: 'ivy-hive', test: 'hive-storage-worker' },
  'tests/rust/hive_worker_client.rs': {
    package: 'ivy-hive', test: 'hive-worker-client',
    prerequisites: [{ platform: 'win32', package: 'ivy-host-runtime', bin: 'ivy-job' }],
  },
  'tests/rust/phone_media.rs': { package: 'ivy-phone-bridge', test: 'phone_media' },
  'tests/rust/phone_diagnostics.rs': { package: 'ivy-phone-bridge', test: 'phone_diagnostics' },
  'tests/rust/phone_native_rpc.rs': { package: 'ivy-phone-bridge', test: 'phone_native_rpc' },
  'tests/secretary-rust/document.rs': { package: 'ivy-secretary', test: 'document' },
  'tests/secretary-rust/journal.rs': { package: 'ivy-secretary', test: 'journal' },
  'tests/secretary-rust/schedule-policy.rs': { package: 'ivy-secretary', test: 'schedule-policy' },
  'tests/secretary-rust/spool.rs': { package: 'ivy-secretary', test: 'spool' },
  'tests/secretary-rust/media-input.rs': { package: 'ivy-secretary', test: 'media-input' },
  'tests/rust/task-board-evidence.rs': { package: 'ivy-task-board', test: 'task-board-evidence' },
  'tests/rust/task-board-operations.rs': { package: 'ivy-task-board', test: 'task-board-operations' },
  'tests/rust/task-board-blobs.rs': { package: 'ivy-task-board', test: 'task-board-blobs' },
  'tests/rust/task-board-output.rs': { package: 'ivy-task-board', test: 'task-board-output' },
};

// Only reviewed, isolated tests may reuse a successful result. Integration tests always run.
export const cacheable = (file) =>
  ["tests/build-cache.test.mjs", "tests/chat-contracts.test.ts"].includes(file);
