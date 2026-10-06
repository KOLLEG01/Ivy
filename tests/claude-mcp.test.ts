import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { ClaudeMcpConfigurationManager } from "../services/agent-manager/src/claude-mcp.js";
import { loadEnvironmentDefaults } from "../services/agent-manager/src/environment-defaults.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import type { ServiceConnection } from "../packages/sdk/src/service.js";

const absent = {
  request: async () => {
    throw new IvyError("not_found", "No override.");
  },
} as unknown as Pick<ServiceConnection, "request">;

test("Claude MCP projection keeps the Hive credential on its own endpoint and detects edits", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-claude-mcp-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const defaults = await loadEnvironmentDefaults("", "https://hive.example");
  const path = join(root, "mcp.json");
  const manager = new ClaudeMcpConfigurationManager(
    path,
    "host-test",
    "https://hive.example",
    "private-token",
    defaults.mcp,
  );
  await manager.synchronize(absent);
  assert.equal(manager.status.state, "applied");
  const projected = JSON.parse(await readFile(path, "utf8")) as Record<
    string,
    { url: string; headers?: { Authorization: string } }
  >;
  assert.equal(projected.ivy?.url, "https://hive.example/mcp");
  assert.equal(projected.ivy?.headers?.Authorization, "Bearer private-token");
  for (const server of Object.values(projected))
    if (server.headers)
      assert.ok(server.url.startsWith("https://hive.example/mcp"));
  await writeFile(path, "{}");
  await manager.synchronize(absent);
  assert.equal(manager.status.state, "conflict");
  assert.equal(await readFile(path, "utf8"), "{}");
  const restarted = new ClaudeMcpConfigurationManager(
    path,
    "host-test",
    "https://hive.example",
    "private-token",
    defaults.mcp,
  );
  await restarted.synchronize(absent);
  assert.equal(restarted.status.state, "conflict");
});
