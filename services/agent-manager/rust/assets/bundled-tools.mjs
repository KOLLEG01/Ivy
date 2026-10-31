// Resolve bundled tools during provisioning. Codex starts the returned commands directly.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";

const anchors = ["manifest.json", "bin/node.exe", "bin/node_repl.exe"];
const hashFile = (file) =>
  createHash("sha256").update(readFileSync(file)).digest("hex");
const requireFile = (file) => {
  if (!statSync(file, { throwIfNoEntry: false })?.isFile())
    throw new Error(`Required runtime file missing: ${file}`);
  return file;
};
const inside = (root, relative) => {
  const file = path.resolve(root, relative),
    local = path.relative(root, file);
  if (!local || local.startsWith("..") || path.isAbsolute(local))
    throw new Error("Runtime manifest path escapes its root");
  return file;
};

export function fingerprintRuntime(bundleRoot) {
  const files = anchors.map((name) => ({
    name,
    sha256: hashFile(requireFile(inside(bundleRoot, name))),
  }));
  const hash = createHash("sha256");
  for (const file of files)
    hash.update(file.name).update("\0").update(file.sha256).update("\0");
  return { id: hash.digest("hex").slice(0, 16), files };
}

export function resolveVerifiedRuntime({ bundleRoot, cacheRoot, app }) {
  const manifest = JSON.parse(
    readFileSync(inside(bundleRoot, "manifest.json"), "utf8"),
  );
  if (
    manifest.platform !== "windows" ||
    manifest.arch !== process.arch ||
    manifest.node_path !== "bin/node.exe" ||
    manifest.node_repl_path !== "bin/node_repl.exe" ||
    manifest.node_modules !== "bin/node_modules"
  )
    throw new Error(
      "Unsupported current app runtime manifest; review this local launcher",
    );
  const fingerprint = fingerprintRuntime(bundleRoot);
  const matches = readdirSync(cacheRoot, { withFileTypes: true })
    .filter((item) => item.isDirectory() && !item.name.startsWith("."))
    .map((item) => inside(cacheRoot, item.name))
    .filter((root) =>
      fingerprint.files.every((file) => {
        const candidate = inside(root, file.name);
        return (
          statSync(candidate, { throwIfNoEntry: false })?.isFile() &&
          hashFile(candidate) === file.sha256
        );
      }),
    );
  if (matches.length !== 1)
    throw new Error(
      "Exactly one runtime cache matching the current app bundle is required",
    );
  const [runtimeRoot] = matches;
  // Validate the launch entry points as well as the app's own identity anchors.
  const entrypoints = [
    "bin/node_modules/@oai/cua-repl/bin/cua-repl.mjs",
    "bin/node_modules/@oai/sky/bin/windows/codex-computer-use.exe",
  ];
  for (const pkg of ["sky", "browser-desktop"]) {
    const relative = `bin/node_modules/@oai/${pkg}/package.json`;
    const metadata = JSON.parse(
      readFileSync(inside(bundleRoot, relative), "utf8"),
    );
    const service = metadata.exports?.["./service"];
    if (typeof service !== "string")
      throw new Error(`Unsupported ${pkg} service export`);
    entrypoints.push(relative, path.join(path.dirname(relative), service));
  }
  for (const relative of entrypoints) {
    if (
      hashFile(requireFile(inside(runtimeRoot, relative))) !==
      hashFile(requireFile(inside(bundleRoot, relative)))
    ) {
      throw new Error(
        `Current app runtime entry point differs from its bundle: ${relative}`,
      );
    }
  }
  return {
    app,
    runtimeRoot,
    bundleRoot,
    runtimeArchiveVersion: manifest.runtime_archive_version,
    fingerprint,
    node: inside(runtimeRoot, manifest.node_path),
    repl: inside(runtimeRoot, manifest.node_repl_path),
    modules: inside(runtimeRoot, manifest.node_modules),
    cua: inside(runtimeRoot, entrypoints[0]),
    notify: inside(runtimeRoot, entrypoints[1]),
  };
}

export function installedRuntime() {
  // Query the current user's Windows package registration directly. Loading the
  // Appx PowerShell module in every parallel MCP connection is slow under load.
  const reg = path.join(
    process.env.SystemRoot ?? "C:/Windows",
    "System32/reg.exe",
  );
  const repository =
    "HKCU\\Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";
  const query = (args) =>
    execFileSync(reg, ["query", ...args], {
      encoding: "utf8",
      windowsHide: true,
      timeout: 10000,
    });
  const matches = query([repository, "/f", "OpenAI.Codex_", "/k"])
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) =>
      /^HKEY_CURRENT_USER\\.*\\OpenAI\.Codex_[^\\]+$/.test(line),
    );
  if (matches.length !== 1)
    throw new Error("Exactly one registered Codex app is required");
  const fullName = matches[0].split("\\").at(-1);
  const identity = fullName.match(
    /^OpenAI\.Codex_(\d+\.\d+\.\d+\.\d+)_(x64|arm64)__2p2nqsd0c76g0$/,
  );
  const installLocation = query([matches[0], "/v", "PackageRootFolder"])
    .match(/PackageRootFolder\s+REG_SZ\s+(.+)/)?.[1]
    .trim();
  if (!identity || !installLocation || !path.isAbsolute(installLocation))
    throw new Error("Unexpected Codex package registration");
  const xml = readFileSync(
    path.join(installLocation, "AppxManifest.xml"),
    "utf8",
  );
  const declared = xml.match(/<Identity\s+[^>]*\/>/)?.[0];
  if (
    !declared?.includes('Name="OpenAI.Codex"') ||
    !declared.includes(`Version="${identity[1]}"`) ||
    !declared.includes(`ProcessorArchitecture="${identity[2]}"`)
  )
    throw new Error("Registered package and Appx manifest disagree");
  requireFile(path.join(installLocation, "AppxSignature.p7x"));
  const app = {
    Name: "OpenAI.Codex",
    Version: identity[1],
    PackageFullName: fullName,
    PackageFamilyName: "OpenAI.Codex_2p2nqsd0c76g0",
    InstallLocation: installLocation,
    selection: "Windows current-user package registration",
  };
  if (!process.env.LOCALAPPDATA) throw new Error("LOCALAPPDATA is required");
  const resolved = resolveVerifiedRuntime({
    app,
    bundleRoot: path.join(app.InstallLocation, "app/resources/cua_node"),
    cacheRoot: path.join(
      process.env.LOCALAPPDATA,
      "OpenAI/Codex/runtimes/cua_node",
    ),
  });
  const current = query([repository, "/f", "OpenAI.Codex_", "/k"])
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) =>
      /^HKEY_CURRENT_USER\\.*\\OpenAI\.Codex_[^\\]+$/.test(line),
    );
  if (
    current.length !== 1 ||
    current[0] !== matches[0] ||
    query([current[0], "/v", "PackageRootFolder"])
      .match(/PackageRootFolder\s+REG_SZ\s+(.+)/)?.[1]
      .trim() !== installLocation
  )
    throw new Error("Registered app changed during runtime verification");
  return resolved;
}

export function buildLaunch(runtime, mode, args, environment = process.env) {
  if (!["node_repl", "cua_repl", "notify"].includes(mode))
    throw new Error("Expected node_repl, cua_repl, notify or --describe");
  if (mode !== "notify" && !path.isAbsolute(environment.CODEX_HOME ?? ""))
    throw new Error("An explicit absolute CODEX_HOME is required");
  const env = {
    ...environment,
    NODE_REPL_NODE_MODULE_DIRS: runtime.modules,
    NODE_REPL_NODE_PATH: runtime.node,
    NODE_REPL_TRUSTED_CODE_PATHS: [environment.CODEX_HOME, runtime.modules]
      .filter(Boolean)
      .join(";"),
    CUA_REPL_NODE_REPL_PATH: runtime.repl,
  };
  return {
    command:
      mode === "node_repl"
        ? runtime.repl
        : mode === "notify"
          ? runtime.notify
          : runtime.node,
    args: mode === "cua_repl" ? [runtime.cua, ...args] : args,
    env,
  };
}

export function directTools(codexHome) {
  if (!path.isAbsolute(codexHome))
    throw new Error("An absolute Codex home is required");
  const runtime = installedRuntime();
  const servers = {};
  const keys = [
    "CODEX_HOME",
    "NODE_REPL_NODE_MODULE_DIRS",
    "NODE_REPL_NODE_PATH",
    "NODE_REPL_TRUSTED_CODE_PATHS",
    "CUA_REPL_NODE_REPL_PATH",
  ];
  for (const mode of ["node_repl", "cua_repl"]) {
    const launch = buildLaunch(runtime, mode, [], {
      ...process.env,
      CODEX_HOME: codexHome,
    });
    servers[mode] = {
      command: launch.command,
      args: launch.args,
      env: Object.fromEntries(keys.map((key) => [key, launch.env[key]])),
    };
  }
  return {
    app: runtime.app.PackageFullName,
    fingerprint: runtime.fingerprint.id,
    servers,
  };
}
if (process.argv[1] === "--ivy-bundled-tools") {
  try {
    process.stdout.write(JSON.stringify(directTools(process.argv[2])));
  } catch {
    process.stderr.write(
      "Bundled tool runtime verification failed. Open the registered app to provision its matching runtime, then retry.",
    );
    process.exitCode = 1;
  }
}
