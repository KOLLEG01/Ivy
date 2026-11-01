import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, cp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import {
  resolveVerifiedRuntime,
  buildLaunch,
} from "../services/agent-manager/rust/assets/bundled-tools.mjs";

test("bundled tools bind the current app fingerprint and launch directly with the required environment", async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), "ivy-bundled-tools-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const bundle = path.join(root, "bundle"),
    caches = path.join(root, "cache"),
    selected = path.join(caches, "selected");
  const files = {
    "manifest.json": JSON.stringify({
      platform: "windows",
      arch: process.arch,
      node_path: "bin/node.exe",
      node_repl_path: "bin/node_repl.exe",
      node_modules: "bin/node_modules",
      runtime_archive_version: "fixture",
    }),
    "bin/node.exe": "node",
    "bin/node_repl.exe": "repl",
    "bin/node_modules/@oai/cua-repl/bin/cua-repl.mjs": "cua",
    "bin/node_modules/@oai/sky/bin/windows/codex-computer-use.exe": "notify",
    "bin/node_modules/@oai/sky/package.json": JSON.stringify({
      exports: { "./service": "./service.js" },
    }),
    "bin/node_modules/@oai/sky/service.js": "sky",
    "bin/node_modules/@oai/browser-desktop/package.json": JSON.stringify({
      exports: { "./service": "./service.js" },
    }),
    "bin/node_modules/@oai/browser-desktop/service.js": "browser",
  };
  for (const [relative, bytes] of Object.entries(files)) {
    const file = path.join(bundle, relative);
    await mkdir(path.dirname(file), { recursive: true });
    await writeFile(file, bytes);
  }
  await mkdir(caches);
  await cp(bundle, selected, { recursive: true });
  const resolve = () =>
    resolveVerifiedRuntime({
      bundleRoot: bundle,
      cacheRoot: caches,
      app: { identity: "registered" },
    });
  const runtime = resolve();
  const environment = {
    CODEX_HOME: root,
    KEEP: "retained",
    NODE_REPL_TRUSTED_SERVICES: "browser",
  };
  const repl = buildLaunch(runtime, "node_repl", ["argument"], environment);
  assert.equal(repl.command, path.join(selected, "bin/node_repl.exe"));
  assert.deepEqual(repl.args, ["argument"]);
  const cua = buildLaunch(runtime, "cua_repl", ["argument"], environment);
  assert.equal(cua.command, path.join(selected, "bin/node.exe"));
  assert.deepEqual(cua.args, [
    path.join(selected, "bin/node_modules/@oai/cua-repl/bin/cua-repl.mjs"),
    "argument",
  ]);
  assert.equal(cua.env.CUA_REPL_NODE_REPL_PATH, repl.command);
  assert.equal(cua.env.NODE_REPL_NODE_PATH, cua.command);
  assert.equal(
    cua.env.NODE_REPL_TRUSTED_CODE_PATHS,
    [root, runtime.modules].join(";"),
  );
  assert.equal(cua.env.NODE_REPL_TRUSTED_SERVICES, "browser");
  assert.equal(cua.env.KEEP, "retained");
  if (process.platform === "win32") {
    for (const mode of ["node_repl", "cua_repl"]) {
      const launch = buildLaunch(runtime, mode, [], {
        ...environment,
        CODEX_HOME: path.toNamespacedPath(root),
      });
      assert.equal(launch.env.CODEX_HOME, root);
      assert.equal((await stat(launch.env.CODEX_HOME)).ino, (await stat(root)).ino);
      assert.equal(
        launch.env.NODE_REPL_TRUSTED_CODE_PATHS,
        [root, runtime.modules].join(";"),
      );
      const unc = buildLaunch(runtime, mode, [], {
        ...environment,
        CODEX_HOME: "\\\\?\\UNC\\server\\share\\name with % and #",
      });
      assert.equal(unc.env.CODEX_HOME, "\\\\server\\share\\name with % and #");
    }
  }
  await writeFile(
    path.join(selected, "bin/node_modules/@oai/browser-desktop/service.js"),
    "wrong",
  );
  assert.throws(resolve, /differs/);
  await cp(bundle, selected, { recursive: true });
  await cp(bundle, path.join(caches, "duplicate"), { recursive: true });
  assert.throws(resolve, /Exactly one/);
  await rm(path.join(caches, "duplicate"), { recursive: true });
  await rm(path.join(selected, "bin/node_repl.exe"));
  assert.throws(resolve, /Exactly one/);
  await writeFile(
    path.join(bundle, "bin/node_modules/@oai/sky/package.json"),
    JSON.stringify({ exports: { "./service": "../../../../../escape.js" } }),
  );
  await cp(bundle, selected, { recursive: true });
  assert.throws(resolve, /escapes/);
});
