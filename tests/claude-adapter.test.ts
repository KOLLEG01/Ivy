import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  assertClaudeSubscriptionConfig,
  checkedClaudeAdapter,
} from "../services/agent-manager/src/claude-adapter.js";
import type { Agent } from "../packages/contracts/src/generated.js";

const catalog = JSON.parse(
  readFileSync("specs/native/codex-0.142.3/catalog.json", "utf8"),
) as Agent.Catalog;

test("Claude adapter catalog exposes only the Ivy lifecycle surface", () => {
  const methods = new Set(catalog.clientRequests.map((value) => value.method));
  for (const method of [
    "initialize",
    "project/list",
    "project/read",
    "project/create",
    "project/update",
    "project/delete",
    "thread/start",
    "thread/resume",
    "thread/list",
    "thread/read",
    "thread/archive",
    "thread/unarchive",
    "thread/turns/list",
    "thread/items/list",
    "thread/loaded/list",
    "turn/start",
    "turn/interrupt",
    "config/read",
    "fs/readFile",
    "permissionProfile/list",
    "collaborationMode/list",
  ])
    assert.ok(methods.has(method), method);
  assert.equal(methods.has("account/login/start"), false);
  assert.equal(methods.has("config/value/write"), false);
  assert.equal(methods.has("thread/fork"), false);
});

test(
  "pinned Claude adapter bundle detects changed executable bytes",
  { skip: !process.env.IVY_CLAUDE_ADAPTER_ROOT },
  async () => {
    const root = process.env.IVY_CLAUDE_ADAPTER_ROOT!;
    assert.ok(
      (await checkedClaudeAdapter(root, catalog.nativeExecutableHash)).endsWith(
        "main.mjs",
      ),
    );
    await assert.rejects(
      checkedClaudeAdapter(root, "sha256:" + "0".repeat(64)),
      { code: "native_build_mismatch" },
    );
  },
);

test("Claude subscription preflight rejects API billing settings without reading values into diagnostics", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-claude-billing-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await assertClaudeSubscriptionConfig(root, {});
  await writeFile(
    join(root, "settings.json"),
    JSON.stringify({ env: { ANTHROPIC_API_KEY: "secret-value" } }),
  );
  await assert.rejects(assertClaudeSubscriptionConfig(root, {}), {
    code: "billing_configuration_conflict",
  });
  await writeFile(join(root, "settings.json"), "{}");
  await assert.rejects(
    assertClaudeSubscriptionConfig(root, { CLAUDE_CODE_USE_BEDROCK: "1" }),
    { code: "billing_configuration_conflict" },
  );
});

test("vendor SDK transport keeps controls live and closes every query once", () => {
  const result = JSON.parse(execFileSync(process.execPath, ['tests/fixtures/agent-claude-bridge-fake.mjs', '--check'], { encoding: 'utf8' }));
  assert.equal(result.allClosedOnce, true);
  assert.equal(result.queries, 6);
});