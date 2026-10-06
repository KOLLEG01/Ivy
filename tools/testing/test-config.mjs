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
  if (/phone-|native-media-adapter/.test(file)) return "phone";
  if (/task-board/.test(file)) return "task-board";
  if (/chat/.test(file)) return "chat";
  if (/secretary/.test(file)) return "secretary";
  if (/automation/.test(file)) return "automation";
  if (/sdk/.test(file)) return "sdk";
  if (/native-|agent-|codex-|claude-|project-locations/.test(file)) return "agent";
  if (/web-e2e\/|ui-(content|live)/.test(file)) return "ui";
  if (
    /kernel|storage|backup|transports|connection-auth|hive-|browser-push|contracts|schema-compiler|concurrency|console-inspection|ui-publication/.test(
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

// Only reviewed, isolated tests may reuse a successful result. Integration tests always run.
export const cacheable = (file) =>
  ["tests/build-cache.test.mjs", "tests/chat-contracts.test.ts"].includes(file);
