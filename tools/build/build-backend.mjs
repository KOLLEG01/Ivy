import { compilerCache } from "./build-cache.mjs";
import { spawnSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
  readdirSync,
} from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { buildIdentity } from "../../packages/host-runtime/src/build-identity.mjs";
import { buildNative, nativeOutputNames } from "./build-native.mjs";

export const componentRoots = {
  "data-collector": ["services/data-collector/src/main.ts", "packages/host-runtime/src/health.ts"],
  dashboards: ["services/dashboards/src/main.ts", "services/dashboards/src/pngjs.d.ts", "packages/host-runtime/src/health.ts"],
  // Entrypoints loaded by path/worker URL are roots as well. This keeps a
  // component package self-contained after the staging directory is cleared.
  hive: [
    "services/hive/src/main.ts",
    "services/hive/src/worker.ts",
    "services/hive/src/settings-recovery.ts",
    "packages/host-runtime/src/health.ts",
  ],
  // The immutable host-executor candidate also owns the operator CLI used by
  // deployment host entries (including runtime-reset-host over SSH).
  "host-executor": [
    "packages/host-runtime/src/executor.ts",
    "packages/host-runtime/src/bootstrap-auto-worker.ts",
    "packages/host-runtime/src/health.ts",
    "packages/cli/src/main.ts",
  ],
  "service-manager": [
    "services/service-manager/src/main.ts",
    "packages/host-runtime/src/health.ts",
  ],
  "agent-manager": [
    "services/agent-manager/src/main.ts",
    "packages/host-runtime/src/health.ts",
  ],
  "chat-bridge": [
    "services/chat-bridge/src/main.ts",
    "packages/host-runtime/src/health.ts",
    "services/chat-bridge/src/whatsapp/audio-worker.ts",
    "services/chat-bridge/src/whatsapp/journal-worker.ts",
  ],
  secretary: [
    "services/secretary/src/main.ts",
    "services/secretary/src/document-worker.ts",
    "services/secretary/src/yauzl.d.ts",
    "packages/host-runtime/src/health.ts",
  ],
  "task-board": [
    "services/task-board/src/main.ts",
    "packages/host-runtime/src/health.ts",
  ],
  "phone-bridge": [
    "services/phone-bridge/src/main.ts",
    "packages/host-runtime/src/health.ts",
  ],
  "automation-example": [
    "docs/examples/services/automation-example/src/main.ts",
    "packages/host-runtime/src/health.ts",
  ],
};

function componentConfig(component, outputRoot, info) {
  const roots = componentRoots[component];
  if (!roots) throw new Error("Unknown backend component: " + component);
  const path = resolve(".local", "build", component + ".tsconfig.json");
  mkdirSync(resolve(".local", "build"), { recursive: true });
  writeFileSync(
    path,
    JSON.stringify(
      {
        extends: resolve("tsconfig.runtime.json"),
        compilerOptions: {
          outDir: resolve(outputRoot),
          tsBuildInfoFile: resolve(info),
          declaration: false,
          sourceMap: false,
        },
        include: [],
        files: roots.map((root) => resolve(root)),
      },
      null,
      2,
    ) + "\n",
  );
  return path;
}

export function compileBackend({ native = true, component = null } = {}) {
  const identity = buildIdentity();
  const outputRoot = component
    ? resolve(".local", "build", component)
    : resolve("dist");
  const info = component
    ? resolve(".local", "build", component + ".tsbuildinfo")
    : resolve("dist/.backend.tsbuildinfo");
  const config = component
    ? componentConfig(component, outputRoot, info)
    : "tsconfig.runtime.json";
  const outputs = component
    ? [resolve(outputRoot, "packages"), resolve(outputRoot, "services"), resolve(outputRoot, "instructions")]
    : [resolve("dist/packages"), resolve("dist/services"), resolve("dist/instructions")];
  if (component && process.argv.includes("--force")) {
    rmSync(outputRoot, { recursive: true, force: true });
    rmSync(info, { force: true });
  }
  if (!component) rmSync("dist/tests", { recursive: true, force: true });
  const finish = compilerCache(info, outputs, process.argv.includes("--force"));
  const result = spawnSync(
    process.execPath,
    [
      "node_modules/typescript/bin/tsc",
      "-p",
      config,
      "--incremental",
      "--tsBuildInfoFile",
      info,
    ],
    { stdio: "inherit", timeout: 120_000, windowsHide: true },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
  // Runtime compilation no longer emits declarations, maps or tests. Remove leftovers
  // from older builds so incremental output cannot silently reintroduce development files.
  for (const base of outputs) pruneRuntimeNoise(base);
  finish();
  if (native) buildNative(component ? { component } : {});
  return identity;
}

function pruneRuntimeNoise(directory) {
  if (!existsSync(directory)) return;
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = directory + "/" + entry.name;
    if (entry.isDirectory()) pruneRuntimeNoise(path);
    else if (/\.(?:map|d\.ts)$/.test(entry.name)) rmSync(path, { force: true });
  }
}

const nativeCatalogComponents = new Set([
  "service-manager",
  "agent-manager",
  "chat-bridge",
  "task-board",
]);
const bootstrapAssetComponents = new Set(["host-executor", "service-manager"]);

/** Stage only inputs that TypeScript emitted for the selected component, plus
 * the few resources opened dynamically at runtime. The explicit all-build
 * remains the complete distribution maintenance path. */
export function stageBackend(identity, component = null) {
  // Replace generated contracts so removed native versions cannot survive an incremental build.
  mkdirSync("dist", { recursive: true });
  if (component) {
    const outputRoot = resolve(".local", "build", component);
    for (const directory of [
      "dist/packages",
      "dist/services",
      "dist/tests",
      "dist/tools",
      "dist/instructions",
    ])
      rmSync(directory, { recursive: true, force: true });
    for (const directory of ["packages", "services", "instructions"])
      if (existsSync(resolve(outputRoot, directory)))
        cpSync(resolve(outputRoot, directory), resolve("dist", directory), {
          recursive: true,
        });
    if (process.platform === "win32" && existsSync("dist/native")) {
      const keep = new Set(nativeOutputNames(component));
      for (const entry of readdirSync("dist/native"))
        if (!keep.has(entry))
          rmSync(resolve("dist/native", entry), {
            recursive: true,
            force: true,
          });
    }
  }
  const target = resolve("dist/specs");
  if (!component || component === "hive" || component === "agent-manager") {
    mkdirSync("dist/instructions", { recursive: true });
    cpSync("instructions/hive-mcp-instructions.md", "dist/instructions/hive-mcp-instructions.md");
    cpSync("instructions/hive-mcp-agent-summary.md", "dist/instructions/hive-mcp-agent-summary.md");
    cpSync("instructions/hive-mcp-examples.md", "dist/instructions/hive-mcp-examples.md");
    cpSync("instructions/ivy-dev-mcp-instructions.md", "dist/instructions/ivy-dev-mcp-instructions.md");
  }
  if (
    realpathSync("dist") !== resolve("dist") ||
    (existsSync(target) && realpathSync(target) !== target)
  )
    throw new Error("Runtime contract output must not use directory links.");
  rmSync(target, { recursive: true, force: true });
  mkdirSync(target, { recursive: true });
  if (component) {
    const emitted = resolve(".local", "build", component, "specs");
    if (existsSync(emitted)) cpSync(emitted, target, { recursive: true });
    if (nativeCatalogComponents.has(component))
      cpSync("specs/native", "dist/specs/native", { recursive: true });
    if (bootstrapAssetComponents.has(component))
      cpSync(
        "packages/host-runtime/assets",
        "dist/packages/host-runtime/assets",
        { recursive: true },
      );
    if (component === "service-manager") {
      mkdirSync("dist/tools/contracts", { recursive: true });
      cpSync(
        "tools/contracts/native-projection.mjs",
        "dist/tools/native-projection.mjs",
      );
      cpSync(
        "tools/contracts/native-projection.mjs",
        "dist/tools/contracts/native-projection.mjs",
      );
    }
  } else {
    for (const directory of ["schemas", "native"])
      cpSync("specs/" + directory, "dist/specs/" + directory, {
        recursive: true,
      });
    mkdirSync("dist/tools/contracts", { recursive: true });
    cpSync(
      "tools/contracts/native-projection.mjs",
      "dist/tools/native-projection.mjs",
    );
    cpSync(
      "tools/contracts/native-projection.mjs",
      "dist/tools/contracts/native-projection.mjs",
    );
    cpSync(
      "packages/host-runtime/assets",
      "dist/packages/host-runtime/assets",
      { recursive: true },
    );
  }
  // The public SDK host entrypoint re-exports this JavaScript runtime helper.
  mkdirSync("dist/packages/host-runtime/src", { recursive: true });
  cpSync(
    "packages/host-runtime/src/build-identity.mjs",
    "dist/packages/host-runtime/src/build-identity.mjs",
  );
  writeFileSync(
    "dist/build-info.json",
    JSON.stringify(identity, null, 2) + "\n",
  );
}

if (
  process.argv[1] &&
  pathToFileURL(resolve(process.argv[1])).href === import.meta.url
) {
  const componentIndex = process.argv.indexOf("--component");
  const component =
    componentIndex >= 0 ? process.argv[componentIndex + 1] : null;
  stageBackend(
    compileBackend({
      native: !process.argv.includes("--no-native"),
      component,
    }),
  );
}
