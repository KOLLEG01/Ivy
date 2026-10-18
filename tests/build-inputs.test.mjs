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
import { buildInstructions } from "../tools/build/build-instructions.mjs";
import { runInNewContext } from "node:vm";
import { isRustRootInput } from "../tools/build/rust-root-inputs.mjs";
import { affected, scopes } from "../tools/testing/test-config.mjs";

test("native source root classification is bounded and root configuration selects affected services", () => {
  for (const path of ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config", ".cargo/config.toml"]) {
    assert.equal(isRustRootInput(path), true, path);
    assert.deepEqual(affected(path), scopes.filter(scope => scope !== "tooling"), path);
  }
  for (const path of ["target/debug/cache", ".local/cargo/cache", ".cargo/credentials", ".cargo/credentials.toml", ".cargo/registry/index", ".cargo/git/cache", ".cargo/config.toml.bak", "nested/Cargo.toml", "../Cargo.toml", "./Cargo.toml", "cargo.toml", ".cargo\\config.toml"]) {
    assert.equal(isRustRootInput(path), false, path);
  }
});

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
    writeFileSync(join(root, "instructions/secretary-notification-policy.ts"), 'export const secretaryNotificationPolicy = "shared policy";\n');
    mkdirSync(join(root, "services/chat-bridge/src"), { recursive: true });
    writeFileSync(join(root, "services/chat-bridge/src/main-context.ts"), 'export const mainInstructions = "Main policy";\n');
    writeFileSync(join(root, "services/chat-bridge/src/notice-input.ts"), 'export const noticePrompt=(callerPrincipalId,text)=>`before ${text} between ${noticePrefix(callerPrincipalId)} after`;\n');
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
    assert.equal(JSON.parse(readFileSync("dist/instructions/secretary-notification-policy.json", "utf8")), "shared policy");
    assert.equal(JSON.parse(readFileSync("dist/instructions/chat-main-instructions.json", "utf8")), "Main policy");
    assert.deepEqual(JSON.parse(readFileSync("dist/instructions/chat-notice-prompt.json", "utf8")), ["before ", " between ", " after"]);
  } finally {
    process.chdir(previous);
    assert.ok(relative(parent, root).startsWith("ivy-build-inputs-"));
    rmSync(root, { recursive: true, force: true });
  }
});

test("notice instruction assets cook only the two approved template expressions", () => {
  const root = mkdtempSync(join(tmpdir(), "ivy-notice-inputs-"));
  const source = join(root, "services/chat-bridge/src/notice-input.ts");
  try {
    mkdirSync(dirname(source), { recursive: true });
    writeFileSync(join(dirname(source), "main-context.ts"), 'export const mainInstructions="Main";');
    const template = '`start\\n\\uD800${text}\\r\\n\\u0000${noticePrefix(callerPrincipalId)}end\\uDFFF`';
    writeFileSync(source, 'export const noticePrompt=(callerPrincipalId:string,text:string):string=>' + template + ';\nthrow new Error("must not execute");');
    const files = buildInstructions({ root, component: "chat-bridge" });
    assert.equal(files.length, 2);
    const parts = JSON.parse(readFileSync(files[1], "utf8"));
    assert.deepEqual(parts, ["start\n\ud800", "\r\n\u0000", "end\udfff"]);
    assert.equal(parts[0] + "message\ud800" + parts[1] + "prefix\udfff" + parts[2], runInNewContext(template, { text: "message\ud800", callerPrincipalId: "caller", noticePrefix: () => "prefix\udfff" }));
    for (const replacement of [template.replace('${text}', '${other}'), template.replace('noticePrefix(callerPrincipalId)', 'noticePrefix(other)'), template.replace('noticePrefix(callerPrincipalId)', 'noticePrefix?.(callerPrincipalId)'), template.replace('${text}', '${text}${extra}')]) {
      writeFileSync(source, 'export const noticePrompt=(callerPrincipalId,text)=>' + replacement + ';');
      assert.throws(() => buildInstructions({ root, component: "chat-bridge" }), /Instruction template/);
      assert.deepEqual(JSON.parse(readFileSync(files[1], "utf8")), parts);
    }
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("instruction assets retain cooked UTF-16 text without executing source modules", () => {
  const parent = tmpdir(), root = mkdtempSync(join(parent, "ivy-instruction-inputs-"));
  const source = join(root, "instructions/secretary-notification-policy.ts");
  const literal = '`start\\n\\uD800\r\nend\\t\\uDFFF\\u0000`';
  try {
    mkdirSync(dirname(source), { recursive: true });
    writeFileSync(source, 'export const secretaryNotificationPolicy: string = ' + literal + ';\nthrow new Error("must not execute");\n');
    const files = buildInstructions({ root, component: "secretary" });
    assert.deepEqual(files, [join(root, "dist/instructions/secretary-notification-policy.json")]);
    const bytes = readFileSync(files[0], "utf8");
    assert.equal(JSON.parse(bytes), runInNewContext(literal));
    assert.ok(bytes.includes('\\ud800') && bytes.includes('\\udfff'));
    assert.equal(bytes.endsWith("\n"), true);
    writeFileSync(source, 'export const secretaryNotificationPolicy = "changed";\n');
    buildInstructions({ root, component: "secretary" });
    assert.equal(JSON.parse(readFileSync(files[0], "utf8")), "changed");
    for (const invalid of [
      'export const secretaryNotificationPolicy = `computed ${1}`;',
      'export let secretaryNotificationPolicy = "mutable";',
      'export const secretaryNotificationPolicy = "unterminated;',
    ]) {
      writeFileSync(source, invalid);
      assert.throws(() => buildInstructions({ root, component: "secretary" }), /Instruction export|Invalid instruction/);
      assert.equal(JSON.parse(readFileSync(files[0], "utf8")), "changed");
    }
    assert.deepEqual(buildInstructions({ root, component: "hive" }), []);
  } finally {
    assert.ok(relative(parent, root).startsWith("ivy-instruction-inputs-"));
    rmSync(root, { recursive: true, force: true });
  }
});

test("the packaged policy equals the canonical shared export", async () => {
  const parent = tmpdir(), root = mkdtempSync(join(parent, "ivy-policy-output-"));
  try {
    const { secretaryNotificationPolicy } = await import("../instructions/secretary-notification-policy.ts");
    const { mainInstructions } = await import("../services/chat-bridge/src/main-context.ts");
    const [file] = buildInstructions({ component: "secretary", outputRoot: root });
    assert.equal(JSON.parse(readFileSync(file, "utf8")), secretaryNotificationPolicy);
    const [main] = buildInstructions({ component: "chat-bridge", outputRoot: root });
    assert.equal(JSON.parse(readFileSync(main, "utf8")), mainInstructions);
  } finally {
    assert.ok(relative(parent, root).startsWith("ivy-policy-output-"));
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
