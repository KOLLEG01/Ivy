import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { PassThrough } from "node:stream";
import test from "node:test";
import { digest } from "../packages/contracts/src/canonical.js";
import type { Agent, Host, Wire } from "../packages/contracts/src/generated.js";
import { HiveClient, callBound, discover } from "../packages/sdk/src/client.js";
import { NativeRpc } from "../services/agent-manager/src/rpc.js";
import { startAgentManager } from "../services/agent-manager/src/main.js";
import type { NativeLauncher } from "../services/agent-manager/src/main.js";
import { HiveServer } from "../services/hive/src/server.js";
import { atomicJson } from "../packages/host-runtime/src/config.js";

test("the same Hive interaction reaches isolated Codex and Claude AgentManager instances", { timeout: 45_000 }, async (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-agent-backends-"));
  const reservation = createServer();
  await new Promise<void>((done) => reservation.listen(0, "127.0.0.1", done));
  const port = (reservation.address() as { port: number }).port;
  await new Promise<void>((done) => reservation.close(() => done()));
  const base = `http://127.0.0.1:${port}/ivy`;
  const build = JSON.parse(readFileSync("dist/build-info.json", "utf8")) as {
    buildId: string;
    version: string;
  };
  const server = new HiveServer({
    filename: join(root, "hive.sqlite"),
    publicBaseUrl: base,
    version: "test",
    buildId: digest("backend-parity"),
    listenPort: port,
    credentials: [
      { principalId: "caller", digest: digest("caller-token") },
      { principalId: "codex-service", digest: digest("codex-token") },
      { principalId: "claude-service", digest: digest("claude-token") },
    ],
    callTimeoutMs: 3000,
  });
  await server.start();
  const managers: Awaited<ReturnType<typeof startAgentManager>>[] = [];
  t.after(async () => {
    for (const manager of managers) await manager.close();
    await server.close();
    rmSync(root, { recursive: true, force: true });
  });
  const backends = [
    { name: "codex", version: "0.154.0", credential: "codex-token" },
    { name: "claude", version: "0.142.3", credential: "claude-token" },
  ] as const;
  const received = new Map<string, number>();
  const client = new HiveClient(base, { credential: "caller-token" });
  for (const backend of backends) {
    const catalog = JSON.parse(
      readFileSync(`specs/native/codex-${backend.version}/catalog.json`, "utf8"),
    ) as Agent.Catalog;
    const dataRoot = join(root, backend.name);
    const nativeHome = join(dataRoot, "native");
    const settings: Agent.Settings = {
      nativeExecutable: process.execPath,
      nativeVersion: backend.version,
      nativeExecutableHash: catalog.nativeExecutableHash,
      codexHome: nativeHome,
      ...(backend.name === "claude"
        ? {
            appServer: {
              mode: "claude-adapter" as const,
              adapterRoot: join(root, "unused-adapter"),
              nodeExecutableHash: digest("node-fixture"),
              claudeExecutable: process.execPath,
              claudeExecutableHash: digest("cli-fixture"),
              defaultModel: "claude-fixture",
            },
          }
        : {}),
      limits: {
        maxOperations: 100,
        maxJournalBytes: 128 * 1024 * 1024,
        maxPendingInputs: 16,
        maxNotificationBytes: 1048576,
      },
    };
    const config: Host.InstanceConfig = {
      schemaVersion: 1,
      componentId: "agent-manager",
      instanceId: backend.name,
      serviceNodeId: `${backend.name}-agent`,
      hostId: "fixture-host",
      publicBaseUrl: base,
      artifactRoot: resolve("."),
      dataRoot,
      buildId: build.buildId,
      version: build.version,
      credential: backend.credential,
      settings,
    };
    const configPath = join(root, `${backend.name}.json`);
    await atomicJson(configPath, config);
    // Native traffic is simulated here; the pinned Claude executable has its own protocol test.
    const launcher: NativeLauncher = async (options) => {
      assert.equal(options.hiveCredential, backend.credential);
      await mkdir(nativeHome, { recursive: true });
      await options.beforeLaunch?.(nativeHome);
      const epoch = randomUUID();
      options.beginEpoch(epoch);
      const input = new PassThrough();
      const output = new PassThrough();
      const rpc = new NativeRpc(catalog, input, output, {
        onClose: (code) => options.onClose(epoch, code),
        onRequest: (value) => options.onRequest(epoch, value),
        onNotification: (value) => options.onNotification(epoch, value),
      });
      let pending = "";
      input.on("data", (chunk: Buffer) => {
        pending += chunk.toString("utf8");
        while (pending.includes("\n")) {
          const end = pending.indexOf("\n");
          const frame = JSON.parse(pending.slice(0, end)) as Record<string, Wire.Json>;
          pending = pending.slice(end + 1);
          if (typeof frame["id"] !== "string" && typeof frame["id"] !== "number") continue;
          if (frame["method"] === "thread/list") {
            const params = frame["params"] as Record<string, Wire.Json>;
            if (params["limit"] === 17)
              received.set(backend.name, (received.get(backend.name) ?? 0) + 1);
            output.write(JSON.stringify({
              id: frame["id"],
              result: {
                data: [],
                nextCursor: params["limit"] === 17 ? backend.name : null,
              },
            }) + "\n");
          } else if (["thread/loaded/list", "project/list", "model/list"].includes(String(frame["method"]))) {
            output.write(JSON.stringify({ id: frame["id"], result: { data: [], nextCursor: null } }) + "\n");
          }
        }
      });
      return {
        rpc,
        epoch,
        catalog,
        initialized: { codexHome: nativeHome },
        nativeHome,
        launcherPid: null,
        close: async () => {
          rpc.close("fixture_stop");
          input.destroy();
          output.destroy();
        },
      };
    };
    const manager = await startAgentManager(configPath, launcher);
    managers.push(manager);
    await manager.service.waitReady({ timeoutMs: 20_000 });
  }
  const epoch = (await client.request("system.status", {})).runtimeEpoch;
  for (const backend of backends) {
    const serviceNodeId = `${backend.name}-agent`;
    const native = await discover(client, "codex.thread/list", { serviceNodeId });
    const interaction = await discover(client, "agent.interact", { serviceNodeId });
    const operationId = `${epoch}:${Date.now()}:${backend.name}-parity`;
    const reply = await callBound(client, interaction, {
      operationId,
      nativeVersion: backend.version,
      method: "thread/list",
      params: { limit: 17 },
      expectedDefinitionHash: native.definitionHash,
    }, operationId) as Agent.Interaction;
    assert.deepEqual(reply.reply, {
      result: { data: [], nextCursor: backend.name },
    });
    assert.equal(received.get(backend.name), 1);
  }
});
