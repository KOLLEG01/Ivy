import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  existsSync,
  rmSync,
} from "node:fs";
import { join, dirname, relative } from "node:path";
import { tmpdir } from "node:os";
import { componentRoots, stageBackend } from "../tools/build/build-backend.mjs";

test("host executor package includes its independent automatic bootstrap worker", () => {
  assert.ok(
    componentRoots["host-executor"].includes(
      "packages/host-runtime/src/bootstrap-auto-worker.ts",
    ),
  );
});

test("incremental backend staging removes retired contracts and preserves current runtime inputs", () => {
  const parent = tmpdir(),
    root = mkdtempSync(join(parent, "ivy-build-inputs-")),
    previous = process.cwd();
  const inputs = [
    "specs/schemas/current.json",
    "specs/native/codex-0.154.0/catalog.json",
    "tools/contracts/native-projection.mjs",
    "packages/host-runtime/assets/launcher",
    "instructions/hive-mcp-instructions.md",
    "instructions/hive-mcp-agent-summary.md",
    "instructions/hive-mcp-examples.md",
    "instructions/ivy-dev-mcp-instructions.md",
    "packages/host-runtime/src/build-identity.mjs",
    "packages/host-runtime/src/build-identity.d.mts",
  ];
  try {
    for (const path of [
      ...inputs,
      "dist/specs/native/codex-0.149.1/catalog.json",
      "dist/specs/schemas/README.md",
    ]) {
      mkdirSync(dirname(join(root, path)), { recursive: true });
      writeFileSync(join(root, path), path);
    }
    process.chdir(root);
    stageBackend({ buildId: "fixture" });
    assert.equal(existsSync("dist/specs/native/codex-0.149.1"), false);
    assert.equal(existsSync("dist/specs/schemas/README.md"), false);
    for (const path of inputs.filter((path) => path.startsWith("specs/")))
      assert.equal(readFileSync("dist/" + path, "utf8"), path);
    assert.equal(readFileSync("dist/instructions/hive-mcp-instructions.md", "utf8"), "instructions/hive-mcp-instructions.md");
    assert.equal(readFileSync("dist/instructions/hive-mcp-agent-summary.md", "utf8"), "instructions/hive-mcp-agent-summary.md");
    assert.equal(readFileSync("dist/instructions/hive-mcp-examples.md", "utf8"), "instructions/hive-mcp-examples.md");
    assert.equal(readFileSync("dist/instructions/ivy-dev-mcp-instructions.md", "utf8"), "instructions/ivy-dev-mcp-instructions.md");
  } finally {
    process.chdir(previous);
    assert.ok(relative(parent, root).startsWith("ivy-build-inputs-"));
    rmSync(root, { recursive: true, force: true });
  }
});

test("component backend staging copies emitted contracts and only declared dynamic resources", () => {
  const parent = tmpdir(),
    root = mkdtempSync(join(parent, "ivy-component-inputs-")),
    previous = process.cwd();
  try {
    for (const [path, value] of [
      [".local/build/hive/services/hive/main.js", "runtime"],
      [".local/build/hive/instructions/hive-mcp.js", "compiled guidance"],
      [".local/build/agent-manager/instructions/hive-mcp.js", "agent guidance"],
      ["instructions/hive-mcp-instructions.md", "guidance"],
      ["instructions/hive-mcp-agent-summary.md", "agent summary"],
      ["instructions/hive-mcp-examples.md", "examples"],
      ["instructions/ivy-dev-mcp-instructions.md", "development guidance"],
      [".local/build/hive/specs/schemas/hive-wire.schema.json", "wire"],
      ["specs/native/codex-current/catalog.json", "native"],
      ["packages/host-runtime/assets/install-windows.ps1", "asset"],
      ["packages/host-runtime/src/build-identity.mjs", "identity"],
      ["tools/contracts/native-projection.mjs", "tool"],
      ["dist/specs/schemas/retired.json", "retired"],
    ]) {
      mkdirSync(dirname(join(root, path)), { recursive: true });
      writeFileSync(join(root, path), value);
    }
    process.chdir(root);
    stageBackend({ buildId: "fixture" }, "hive");
    assert.equal(readFileSync("dist/services/hive/main.js", "utf8"), "runtime");
    assert.equal(readFileSync("dist/instructions/hive-mcp.js", "utf8"), "compiled guidance");
    assert.equal(readFileSync("dist/instructions/hive-mcp-instructions.md", "utf8"), "guidance");
    assert.equal(readFileSync("dist/instructions/hive-mcp-agent-summary.md", "utf8"), "agent summary");
    assert.equal(readFileSync("dist/instructions/hive-mcp-examples.md", "utf8"), "examples");
    assert.equal(readFileSync("dist/instructions/ivy-dev-mcp-instructions.md", "utf8"), "development guidance");
    assert.equal(
      readFileSync("dist/specs/schemas/hive-wire.schema.json", "utf8"),
      "wire",
    );
    for (const path of [
      "dist/specs/schemas/retired.json",
      "dist/specs/native",
      "dist/packages/host-runtime/assets",
      "dist/tools/native-projection.mjs",
      "dist/tools/contracts/native-projection.mjs",
    ])
      assert.equal(existsSync(path), false);
    stageBackend({ buildId: "fixture" }, "agent-manager");
    assert.equal(readFileSync("dist/instructions/hive-mcp.js", "utf8"), "agent guidance");
    assert.equal(readFileSync("dist/instructions/hive-mcp-instructions.md", "utf8"), "guidance");
    assert.equal(readFileSync("dist/instructions/hive-mcp-agent-summary.md", "utf8"), "agent summary");
  } finally {
    process.chdir(previous);
    assert.ok(relative(parent, root).startsWith("ivy-component-inputs-"));
    rmSync(root, { recursive: true, force: true });
  }
});
