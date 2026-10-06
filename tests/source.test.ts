import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  readFile,
  writeFile,
  rm,
  unlink,
  chmod,
  stat,
} from "node:fs/promises";
import { join, relative, resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";
import {
  captureSource,
  verifySnapshot,
} from "../packages/host-runtime/src/source.js";
import { runCommand } from "../packages/host-runtime/src/process.js";
import { HostJournal } from "../packages/host-runtime/src/journal.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import type { Host } from "../packages/contracts/src/generated.js";

async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ivy-source-test-")),
    source = join(root, "checkout");
  const git = spawnSync(
    process.platform === "win32" ? "where.exe" : "which",
    ["git"],
    { encoding: "utf8", windowsHide: true, timeout: 5000 },
  )
    .stdout.trim()
    .split(/\r?\n/)[0]!;
  assert.ok(git, "Git is required by source preparation");
  const config: Host.HostConfig = {
    schemaVersion: 1,
    hostId: "source-fixture",
    runtimeRoot: join(root, "state"),
    artifactRoot: join(root, "artifacts"),
    stagingRoot: join(root, "staging"),
    publicBaseUrl: "http://127.0.0.1:39081/ivy",
    executables: { git, node: process.execPath },
    instances: [
      {
        instanceId: "fixture",
        serviceNodeId: "fixture",
        componentId: "fixture",
        enabled: true,
        engine: "process",
        settings: {},
      },
    ],
  };
  await mkdir(join(source, "packages"), { recursive: true });
  await writeFile(
    join(source, "package.json"),
    JSON.stringify({
      name: "source-fixture",
      version: "1.0.0",
      private: true,
      type: "module",
      packageManager: "npm@11.16.0",
    }),
  );
  await writeFile(
    join(source, "package-lock.json"),
    JSON.stringify({
      name: "source-fixture",
      version: "1.0.0",
      lockfileVersion: 3,
    }),
  );
  await writeFile(join(source, "LICENSE"), "Synthetic fixture license\n");
  await writeFile(
    join(source, ".gitignore"),
    "config.json\n.env\n/runtime/\nnode_modules/\n",
  );
  await writeFile(
    join(source, "packages/kept.ts"),
    "export const answer = 1;\n",
  );
  await writeFile(
    join(source, "packages/deleted.ts"),
    "delete before capture\n",
  );
  await runCommand(
    { executable: "git", args: ["init", "--quiet"], timeoutMs: 5000 },
    source,
    config.executables,
    { jobLauncher: resolve("dist/native/ivy-job.exe") },
  );
  await runCommand(
    { executable: "git", args: ["add", "."], timeoutMs: 5000 },
    source,
    config.executables,
    { jobLauncher: resolve("dist/native/ivy-job.exe") },
  );
  await writeFile(
    join(source, "packages/kept.ts"),
    "export const answer = 2;\n",
  );
  await writeFile(join(source, "packages/new.ts"), "new uncommitted module\n");
  await mkdir(join(source, "services/example/src/runtime"), {
    recursive: true,
  });
  await writeFile(
    join(source, "services/example/src/runtime/module.ts"),
    "export const runtimeSource = true;\n",
  );
  await mkdir(join(source, "instructions/skills/example"), { recursive: true });
  await writeFile(
    join(source, "instructions/skills/example/SKILL.md"),
    "# Example runtime instruction\n",
  );
  await mkdir(join(source, ".agents"), { recursive: true });
  await writeFile(
    join(source, ".agents/private-settings.json"),
    '{"excluded":"synthetic-private-data"}\n',
  );
  if (process.platform !== "win32")
    await chmod(join(source, "packages/new.ts"), 0o755);
  await unlink(join(source, "packages/deleted.ts"));
  await writeFile(
    join(source, "config.json"),
    JSON.stringify({ token: "synthetic-excluded-secret" }),
  );
  const journal = new HostJournal(config);
  t.after(async () => {
    journal.close();
    assert.ok(relative(tmpdir(), root).startsWith("ivy-source-test-"));
    await rm(root, { recursive: true, force: true });
  });
  return { root, source, config, journal };
}

test(
  "source handoff fixes intended uncommitted additions/edits/deletions and excludes runtime configuration",
  { timeout: 20_000 },
  async (t) => {
    const f = await fixture(t),
      snapshot = await captureSource(f.source, f.config);
    await verifySnapshot(snapshot, f.config);
    assert.ok(
      (await stat(join(snapshot.sourceRoot, "packages/new.ts"))).isFile(),
    );
    assert.equal(
      await readFile(
        join(snapshot.sourceRoot, "services/example/src/runtime/module.ts"),
        "utf8",
      ),
      "export const runtimeSource = true;\n",
    );
    assert.ok(
      (
        await stat(
          join(snapshot.sourceRoot, "instructions/skills/example/SKILL.md"),
        )
      ).isFile(),
    );
    assert.equal(
      await readFile(join(snapshot.sourceRoot, "LICENSE"), "utf8"),
      "Synthetic fixture license\n",
    );
    for (const excluded of [
      ".agents/private-settings.json",
      "packages/deleted.ts",
      "config.json",
    ])
      await assert.rejects(stat(join(snapshot.sourceRoot, excluded)), {
        code: "ENOENT",
      });
    if (process.platform !== "win32")
      assert.equal(
        (await stat(join(snapshot.sourceRoot, "packages/new.ts"))).mode & 0o777,
        0o755,
      );
    assert.equal(
      await readFile(join(snapshot.sourceRoot, "packages/kept.ts"), "utf8"),
      "export const answer = 2;\n",
    );
    const request = {
      action: "deploy" as const,
      operationId: "snapshot-handoff",
      instanceId: "fixture",
      source: f.source,
    };
    const accepted = f.journal.accept(request, snapshot);
    assert.equal(accepted.record.phase, "preparing");
    assert.equal(accepted.record.targetBuild, null);
    await writeFile(
      join(f.source, "packages/kept.ts"),
      "export const answer = 3;\n",
    );
    assert.deepEqual(f.journal.accept(request), accepted);
    assert.equal(
      await readFile(
        join(accepted.sourceSnapshot!.sourceRoot, "packages/kept.ts"),
        "utf8",
      ),
      "export const answer = 2;\n",
    );
    const next = await captureSource(f.source, f.config);
    assert.notEqual(next.snapshotId, snapshot.snapshotId);
  },
);

test(
  "source handoff preserves selected bytes despite Git attributes and remains independent after publication",
  { timeout: 20_000 },
  async (t) => {
    const f = await fixture(t),
      crlf = Buffer.from("first\r\nsecond\r\n"),
      binary = Buffer.from([0, 13, 10, 255, 128, 1]);
    await writeFile(join(f.source, ".gitattributes"), "* text=auto eol=lf\n");
    await writeFile(join(f.source, "packages/crlf.txt"), crlf);
    await writeFile(join(f.source, "packages/binary.bin"), binary);
    const snapshot = await captureSource(f.source, f.config);
    assert.deepEqual(
      await readFile(join(snapshot.sourceRoot, "packages/crlf.txt")),
      crlf,
    );
    assert.deepEqual(
      await readFile(join(snapshot.sourceRoot, "packages/binary.bin")),
      binary,
    );
    await writeFile(
      join(f.source, "packages/crlf.txt"),
      "changed after capture\n",
    );
    await writeFile(
      join(f.source, "packages/binary.bin"),
      Buffer.from([2, 3, 4]),
    );
    assert.deepEqual(
      await readFile(join(snapshot.sourceRoot, "packages/crlf.txt")),
      crlf,
    );
    assert.deepEqual(
      await readFile(join(snapshot.sourceRoot, "packages/binary.bin")),
      binary,
    );
  },
);

test(
  "source handoff metadata stays fixed without rescanning copied payloads and rejects unsafe runtime roots",
  { timeout: 20_000 },
  async (t) => {
    const f = await fixture(t);
    assert.throws(
      () =>
        f.journal.accept({
          action: "deploy",
          operationId: "without-snapshot",
          instanceId: "fixture",
          source: f.source,
        }),
      (error: unknown) =>
        error instanceof IvyError && error.code === "source_snapshot_required",
    );
    const snapshot = await captureSource(f.source, f.config);
    await writeFile(
      join(snapshot.sourceRoot, "packages/kept.ts"),
      "modified after capture\n",
    );
    await verifySnapshot(snapshot, f.config);
    await assert.rejects(
      captureSource(f.config.stagingRoot, f.config),
      (error: unknown) =>
        error instanceof IvyError && error.code === "target_conflict",
    );
  },
);

test(
  "a web-only compiler change produces a new handoff and preserves the original web configuration",
  { timeout: 20_000 },
  async (t) => {
    const f = await fixture(t),
      original = JSON.stringify({
        compilerOptions: { strict: true },
        include: ["ui/**/*.vue"],
      });
    await writeFile(join(f.source, "tsconfig.web.json"), original);
    const first = await captureSource(f.source, f.config);
    assert.equal(
      await readFile(join(first.sourceRoot, "tsconfig.web.json"), "utf8"),
      original,
    );
    await verifySnapshot(first, f.config);
    await writeFile(
      join(f.source, "tsconfig.web.json"),
      JSON.stringify({
        compilerOptions: { strict: false },
        include: ["ui/**/*.vue"],
      }),
    );
    const second = await captureSource(f.source, f.config);
    assert.notEqual(first.snapshotId, second.snapshotId);
    assert.equal(
      await readFile(join(first.sourceRoot, "tsconfig.web.json"), "utf8"),
      original,
    );
  },
);

test(
  "independent service source snapshots retain exact dependency lock bytes and detect replacement",
  { timeout: 20_000 },
  async (t) => {
    const f = await fixture(t);
    const bytes = Buffer.from(
      '{"name":"source-fixture","version":"1.0.0","lockfileVersion":3,"packages":{}}\n',
    );
    await writeFile(join(f.source, "package-lock.json"), bytes);
    const first = await captureSource(f.source, f.config);
    assert.deepEqual(
      await readFile(join(first.sourceRoot, "package-lock.json")),
      bytes,
    );
    await verifySnapshot(first, f.config);
    await writeFile(
      join(f.source, "package-lock.json"),
      Buffer.from(
        '{"name":"source-fixture","version":"1.0.1","lockfileVersion":3,"packages":{}}\n',
      ),
    );
    const second = await captureSource(f.source, f.config);
    assert.notEqual(second.snapshotId, first.snapshotId);
    assert.deepEqual(
      await readFile(join(first.sourceRoot, "package-lock.json")),
      bytes,
    );
  },
);

test(
  "independent browser service snapshots retain extension entry points and manifests while excluding generated and private files",
  { timeout: 20_000 },
  async (t) => {
    const f = await fixture(t),
      extension = join(f.source, "services/companion/extension");
    await mkdir(join(extension, "generated"), { recursive: true });
    await writeFile(join(extension, ".gitignore"), "generated/\n");
    const worker = "export const ownedBrowser = true;\n",
      manifest = JSON.stringify({
        manifest_version: 3,
        name: "Own browser companion",
        version: "1.0.0",
        background: { service_worker: "service-worker.js", type: "module" },
      });
    await writeFile(join(extension, "service-worker.mjs"), worker);
    await writeFile(join(extension, "manifest.json"), manifest);
    await writeFile(
      join(extension, "generated/validator.cjs"),
      "generated bytes are not source\n",
    );
    await writeFile(
      join(extension, "config.json"),
      '{"token":"synthetic-excluded-secret"}',
    );
    await mkdir(join(f.source, "services/companion/native/phone-runtime"), {
      recursive: true,
    });
    await writeFile(
      join(f.source, "services/companion/native/phone-runtime/entry.cs"),
      "public static class Entry {}\n",
    );
    const first = await captureSource(f.source, f.config);
    await verifySnapshot(first, f.config);
    assert.equal(
      await readFile(
        join(
          first.sourceRoot,
          "services/companion/extension/service-worker.mjs",
        ),
        "utf8",
      ),
      worker,
    );
    assert.equal(
      await readFile(
        join(first.sourceRoot, "services/companion/extension/manifest.json"),
        "utf8",
      ),
      manifest,
    );
    await assert.rejects(
      stat(
        join(
          first.sourceRoot,
          "services/companion/extension/generated/validator.cjs",
        ),
      ),
      { code: "ENOENT" },
    );
    await assert.rejects(
      stat(join(first.sourceRoot, "services/companion/extension/config.json")),
      { code: "ENOENT" },
    );
    assert.equal(
      await readFile(
        join(
          first.sourceRoot,
          "services/companion/native/phone-runtime/entry.cs",
        ),
        "utf8",
      ),
      "public static class Entry {}\n",
    );
    await writeFile(
      join(extension, "service-worker.mjs"),
      worker + "export const revision = 2;\n",
    );
    const second = await captureSource(f.source, f.config);
    assert.notEqual(second.snapshotId, first.snapshotId);
    assert.equal(
      await readFile(
        join(
          first.sourceRoot,
          "services/companion/extension/service-worker.mjs",
        ),
        "utf8",
      ),
      worker,
    );
    await writeFile(
      join(second.sourceRoot, "services/companion/extension/manifest.json"),
      "{}",
    );
    await verifySnapshot(second, f.config);
  },
);
