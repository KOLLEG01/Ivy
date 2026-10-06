import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { buildIdentity } from "../../packages/host-runtime/src/build-identity.mjs";
import { artifactFiles } from "../../dist/packages/host-runtime/src/artifact.js";
import {
  preparePhoneEvs,
  installPhoneEvs,
} from "../package/phone-evs-package.mjs";
import { fingerprint } from "./build-cache.mjs";

// Run after the ordinary backend build, in the fresh source snapshot used by host preparation.
// Package only: no test execution, device opening, Desktop action or service activation.
assert.equal(process.platform, "win32");
assert.equal(process.arch, "x64");
const root = realpathSync(fileURLToPath(new URL("../../", import.meta.url)));
const projectRoot = join(root, "services/phone-bridge/native/phone-runtime"),
  target = join(root, "dist/native/phone");
const dotnet = process.env.IVY_DOTNET ?? "dotnet",
  identity = buildIdentity(root),
  originalPackage = readFileSync(join(root, "package.json"));
assert.equal(
  existsSync(target),
  false,
  "Phone publish requires a fresh artifact destination.",
);
const work = join(root, ".local/phone-publish-" + randomUUID());
mkdirSync(work, { recursive: true });
const contained = relative(root, realpathSync(work));
assert.ok(
  contained && !isAbsolute(contained) && !contained.startsWith(".."),
  "Publish work must stay inside its source snapshot.",
);
const published = join(work, "files");
function run(name, args, timeout) {
  const result = spawnSync(dotnet, args, {
    cwd: projectRoot,
    encoding: "utf8",
    windowsHide: true,
    timeout,
    maxBuffer: 4 * 1024 * 1024,
    env: {
      ...process.env,
      DOTNET_CLI_TELEMETRY_OPTOUT: "1",
      DOTNET_NOLOGO: "1",
    },
  });
  writeFileSync(join(work, name + ".stdout.log"), result.stdout ?? "");
  writeFileSync(join(work, name + ".stderr.log"), result.stderr ?? "");
  if (result.error || result.status !== 0)
    throw (
      result.error ??
      new Error("Phone " + name + " failed; original logs: " + work)
    );
  return result.stdout;
}
assert.equal(run("sdk", ["--version"], 15000).trim(), "10.0.302");
const lockFile = join(projectRoot, "packages.win-x64.lock.json"),
  originalLock = readFileSync(lockFile);
const lock = JSON.parse(originalLock),
  noticeIndex = JSON.parse(
    readFileSync(join(projectRoot, "licenses/dependencies.json"), "utf8"),
  );
const expected = new Set(
  Object.values(lock.dependencies).flatMap((packages) =>
    Object.entries(packages).map(([id, value]) => id + "/" + value.resolved),
  ),
);
expected.add("Microsoft.NETCore.App.Runtime.win-x64/10.0.10");
assert.deepEqual(
  Object.keys(noticeIndex).sort(),
  [...expected].sort(),
  "Every selected dependency requires its exact reviewed notices.",
);
for (const names of Object.values(noticeIndex)) {
  assert.ok(Array.isArray(names) && names.length > 0);
  for (const name of names) {
    assert.match(name, /^[A-Za-z0-9._-]+\.(?:txt|md)$/);
    assert.ok(
      readFileSync(join(projectRoot, "licenses", name)).length > 100,
      "Complete license text/notice required.",
    );
  }
}
const projectInputs = readdirSync(projectRoot, { withFileTypes: true })
  .filter((entry) => !["bin", "obj"].includes(entry.name))
  .map((entry) => join(projectRoot, entry.name));
const input = fingerprint(
  [
    ...projectInputs,
    join(root, "LICENSE"),
    join(root, "services/phone-bridge/native/input"),
    join(root, "services/phone-bridge/native/evs"),
    join(root, "tools/build/build-phone-runtime.mjs"),
    join(root, "tools/build/build-phone-evs.mjs"),
    join(root, "tools/package/phone-evs-package.mjs"),
  ],
  { dotnet: "10.0.302", runtime: "Microsoft.NETCore.App/10.0.10" },
  root,
);
const cacheRoot = process.env.IVY_BUILD_CACHE_ROOT;
assert.ok(
  cacheRoot === undefined || isAbsolute(cacheRoot),
  "Selected native build cache must be absolute.",
);
const cache = cacheRoot ? join(cacheRoot, "phone-runtime", input) : null;
let cacheHit = false;
if (cache)
  try {
    const record = JSON.parse(readFileSync(join(cache, "result.json"), "utf8")),
      files = join(cache, "files");
    if (
      record.input === input &&
      record.output === fingerprint([files], {}, files)
    ) {
      cpSync(files, published, { recursive: true });
      cacheHit = true;
    }
  } catch {
    rmSync(published, {
      recursive: true,
      force: true,
    }); /* Missing or damaged local compiler cache is a normal miss. */
  }
let evsLibraryHash;
if (!cacheHit) {
  // A cache miss must be reproducible from selected source and locked packages alone. In particular,
  // never let developer or interrupted incremental MSBuild state influence the published executable.
  for (const name of ["bin", "obj"])
    rmSync(join(projectRoot, name), { recursive: true, force: true });
  run(
    "restore",
    [
      "restore",
      "Ivy.PhoneRuntime.csproj",
      "--locked-mode",
      "--configfile",
      "NuGet.Config",
      "-p:PublishProfile=win-x64",
    ],
    180000,
  );
  assert.deepEqual(
    readFileSync(lockFile),
    originalLock,
    "Locked restore cannot change the reviewed dependency graph.",
  );
  // Framework download packs are not listed in NuGet's normal dependency lock. Pin their actual
  // archive bytes as well as RuntimeFrameworkVersion before allowing publish to consume them.
  const assets = JSON.parse(
    readFileSync(join(projectRoot, "obj/project.assets.json"), "utf8"),
  );
  const pack =
    "microsoft.netcore.app.runtime.win-x64/10.0.10/microsoft.netcore.app.runtime.win-x64.10.0.10.nupkg";
  const restored = Object.keys(assets.packageFolders)
    .map((folder) => join(folder, pack))
    .find(existsSync);
  assert.ok(restored, "The exact runtime pack must have been restored.");
  assert.equal(
    createHash("sha512").update(readFileSync(restored)).digest("base64"),
    readFileSync(join(projectRoot, "runtime-pack.sha512"), "utf8").trim(),
    "Bundled runtime archive differs from its reviewed bytes.",
  );
  run(
    "publish",
    [
      "publish",
      "Ivy.PhoneRuntime.csproj",
      "--no-restore",
      "-c",
      "Release",
      "-p:PublishProfile=win-x64",
      "--output",
      published,
    ],
    180000,
  );
  const evs = await preparePhoneEvs(root);
  await installPhoneEvs(root, published, evs);
  evsLibraryHash = evs.report.librarySha256;
  cpSync(join(root, "LICENSE"), join(published, "LICENSE"));
  if (cache) {
    mkdirSync(dirname(cache), { recursive: true });
    const incoming = cache + ".incoming-" + randomUUID();
    mkdirSync(incoming);
    cpSync(published, join(incoming, "files"), { recursive: true });
    writeFileSync(
      join(incoming, "result.json"),
      JSON.stringify({
        input,
        output: fingerprint(
          [join(incoming, "files")],
          {},
          join(incoming, "files"),
        ),
      }) + "\n",
    );
    try {
      renameSync(incoming, cache);
    } catch (error) {
      rmSync(incoming, { recursive: true, force: true });
      if (!["EEXIST", "ENOTEMPTY", "EPERM"].includes(error.code)) throw error;
    }
  }
} else {
  const provenance = JSON.parse(
    readFileSync(join(published, "evs-build.json"), "utf8"),
  );
  evsLibraryHash = provenance.librarySha256;
}
const runtime = JSON.parse(
  readFileSync(join(published, "Ivy.PhoneRuntime.runtimeconfig.json"), "utf8"),
).runtimeOptions;
assert.equal(runtime.framework, undefined);
assert.equal(runtime.frameworks, undefined);
assert.deepEqual(runtime.includedFrameworks, [
  { name: "Microsoft.NETCore.App", version: "10.0.10" },
]);
for (const name of [
  "Ivy.PhoneRuntime.exe",
  "Ivy.PhoneRuntime.dll",
  "Ivy.PhoneRuntime.deps.json",
  "coreclr.dll",
  "hostfxr.dll",
  "hostpolicy.dll",
])
  assert.ok(
    readFileSync(join(published, name)).length > 0,
    "Native release is missing " + name,
  );
for (const names of Object.values(noticeIndex))
  for (const name of names)
    assert.deepEqual(
      readFileSync(join(published, "licenses", name)),
      readFileSync(join(projectRoot, "licenses", name)),
      "Published notice changed.",
    );
assert.deepEqual(
  readFileSync(join(published, "licenses/CodexMicro-MIT.txt")),
  readFileSync(join(projectRoot, "licenses/CodexMicro-MIT.txt")),
  "Micro interoperability attribution must ship with the native runtime.",
);
assert.deepEqual(
  readFileSync(join(published, "LICENSE")),
  readFileSync(join(root, "LICENSE")),
  "Project MIT license must ship with the native runtime.",
);
assert.deepEqual(
  readFileSync(join(root, "package.json")),
  originalPackage,
  "Release identity input changed during native publish.",
);
mkdirSync(dirname(target), { recursive: true });
const parent = relative(root, realpathSync(dirname(target)));
assert.ok(
  parent && !parent.startsWith("..") && !isAbsolute(parent),
  "Release parent must stay inside the source snapshot.",
);
assert.equal(
  existsSync(target),
  false,
  "Another build already owns this destination.",
);
renameSync(published, target); // One fresh directory, with no merge or replacement of prior files.
const files = await artifactFiles(target),
  executable = files.find((file) => file.path === "Ivy.PhoneRuntime.exe");
const report = {
  ...identity,
  runtime: "Microsoft.NETCore.App/10.0.10",
  runtimeIdentifier: "win-x64",
  selfContained: true,
  evsLibraryHash: "sha256:" + evsLibraryHash,
  cacheHit,
  path: target,
  executableHash: executable.hash,
  fileCount: files.length,
  bytes: files.reduce((sum, file) => sum + file.bytes, 0),
  logs: work,
};
writeFileSync(
  join(work, "result.json"),
  JSON.stringify(report, null, 2) + "\n",
);
process.stdout.write(JSON.stringify(report) + "\n");
