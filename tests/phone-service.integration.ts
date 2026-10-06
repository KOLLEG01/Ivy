import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join, relative, resolve } from "node:path";
import { tmpdir } from "node:os";
import { createServer } from "node:net";
import { randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { digest } from "../packages/contracts/src/canonical.js";
import { HiveServer } from "../services/hive/src/server.js";
import { HiveClient, serviceTools } from "../packages/sdk/src/client.js";
import { atomicJson, jsonFile } from "../packages/host-runtime/src/config.js";
import { fileHash } from "../packages/host-runtime/src/artifact.js";
import { checkHealth } from "../packages/host-runtime/src/health.js";
import { startPhoneBridge } from "../services/phone-bridge/src/main.js";
import { phoneTools } from "../services/phone-bridge/src/runtime/registry.js";
import { until } from "./fixtures/host.js";

// Explicit Windows integration entrypoint, outside the ordinary platform-independent *.test.js
// glob. Requires a complete published Phone artifact. No Desktop/audio action or SIP peer exists.
test(
  "real Hive and native Phone service preserve their owner through reconnect and retire it on restart",
  { timeout: 90000 },
  async (t) => {
    assert.equal(process.platform, "win32");
    assert.equal(process.arch, "x64");
    const root = await mkdtemp(join(tmpdir(), "ivy-phone-service-")),
      artifactRoot = resolve(".");
    const build = JSON.parse(
      await readFile(join(artifactRoot, "dist/build-info.json"), "utf8"),
    ) as { version: string; buildId: string };
    const executable = "dist/native/phone/Ivy.PhoneRuntime.exe",
      executableHash = await fileHash(join(artifactRoot, executable));
    const listener = createServer();
    await new Promise<void>((done) => listener.listen(0, "127.0.0.1", done));
    const port = (listener.address() as { port: number }).port;
    await new Promise<void>((done) => listener.close(() => done()));
    const base = `http://127.0.0.1:${port}/ivy`,
      dataRoot = join(root, "phone");
    const hive = new HiveServer({
      filename: join(root, "hive.sqlite"),
      publicBaseUrl: base,
      listenHost: "127.0.0.1",
      listenPort: port,
      version: "fixture",
      buildId: digest("phone-service-hive"),
      credentials: ["admin", "outsider", "phone-owner"].map((principalId) => ({
        principalId,
        digest: digest(principalId + "-fixture-token"),
      })),
    });
    let running: Awaited<ReturnType<typeof startPhoneBridge>> | null = null;
    t.after(async () => {
      try {
        await running?.close();
      } finally {
        await hive.close();
        assert.ok(relative(tmpdir(), root).startsWith("ivy-phone-service-"));
        await rm(root, { recursive: true, force: true });
      }
    });
    await hive.start();
    const config = {
      schemaVersion: 1 as const,
      hostId: "fixture-host",
      instanceId: "phone",
      serviceNodeId: "phone",
      componentId: "phone-bridge",
      publicBaseUrl: base,
      dataRoot,
      artifactRoot,
      version: build.version,
      buildId: build.buildId,
      credential: "phone-owner-fixture-token",
      settings: {
        native: { executable, executableHash },
        binding: { address: "127.0.0.1", port: 0, transport: "udp" },
        codecs: { preferences: ["G722"], packetMs: 20 },
        registration: null,
        credentialsPath: null,
        policy: { recipients: [], incoming: [] },
        incomingPrincipalId: null,
        application: {
          appUserModelId: "Fixture.UnusedDesktop!UI",
          startIfMissing: false,
        },
        audio: {
          captureEndpointId: "unused-capture",
          renderEndpointId: "unused-render",
          sourceMode: "desktop_process",
          captureBufferMs: 20,
          renderLatencyMs: 20,
          queueMs: 40,
        },
        incomingRoute: "windows",
        outgoingRoute: "windows",
        voiceArchive: null,
        voiceInput: {
          micro: {
            usbipExecutable: "C:\\verified\\usbip.exe",
            executableHash: "sha256:" + "0".repeat(64),
            port: 3241,
          },
          hotkeyFallback: {
            modifiers: ["control", "shift"],
            keyCode: 32,
            focusBeforeHotkey: false,
          },
        },
        ringSeconds: 30,
        pollMs: 500,
      },
    };
    const path = join(root, "phone.json");
    await atomicJson(path, config);
    running = await startPhoneBridge(path);
    await running.service.waitReady();
    const requirement = [
      { namespace: "phone", interfaceVersion: "1.0.0" },
    ] as const;
    const admin = new HiveClient(base, { credential: "admin-fixture-token" }),
      tools = serviceTools(admin, "phone", requirement);
    const outsider = serviceTools(
      new HiveClient(base, { credential: "outsider-fixture-token" }),
      "phone",
      requirement,
    );
    const status = (await tools.call("phone.status", {})) as {
      epoch: string;
      busy: boolean;
      recipients: string[];
      call: unknown;
      incoming: unknown;
    };
    assert.equal(status.epoch, running.journal.epoch);
    assert.equal(status.busy, false);
    assert.deepEqual(status.recipients, []);
    assert.equal(status.call, null);
    assert.equal(status.incoming, null);
    const codecs = (await tools.call("phone.codecTest", {})) as {
      passed: boolean;
      codecs: {
        name: string;
        passed: boolean;
        frames: number;
        error: string | null;
      }[];
    };
    assert.equal(
      codecs.passed,
      true,
      "Published native artifact must execute every codec, including its shipped EVS DLL.",
    );
    assert.deepEqual(
      codecs.codecs.map((codec) => codec.name),
      ["G722", "PCMA", "PCMU", "OPUS", "EVS"],
    );
    assert.ok(
      codecs.codecs.every(
        (codec) => codec.passed && codec.frames === 25 && codec.error === null,
      ),
    );
    assert.deepEqual(await tools.call("phone.codecTest", {}), codecs);
    assert.deepEqual(
      await outsider.call("phone.codecTest", {}),
      codecs,
      "Every authenticated caller can use the bounded synthetic diagnostic.",
    );
    assert.equal(
      (
        await admin.request("tools.list", {
          namespace: "phone",
          serviceNodeId: "phone",
        })
      ).items.length,
      phoneTools.length,
    );
    assert.equal(
      ((await outsider.call("phone.status", {})) as { epoch: string }).epoch,
      status.epoch,
    );
    await assert.rejects(
      tools.call(
        "phone.request",
        { operationId: randomUUID(), recipientId: "unapproved" },
        randomUUID(),
      ),
      { code: "phone_destination_refused" },
    );
    assert.equal(running.journal.status().operations, 1);
    assert.equal(running.journal.status().calls, 0);
    const owner = await jsonFile<{
      epoch: string;
      pid: number;
      creationFileTime: string;
    }>(join(dataRoot, "phone-native-owner.json"));
    assert.equal(owner.epoch, status.epoch);
    assert.ok(owner.pid > 0);
    await checkHealth(config);
    const generation = running.service.connection.generation;
    running.service.connection.close();
    await until(
      () =>
        !!running?.service.ready &&
        running.service.connection.generation > generation,
      30000,
    );
    assert.deepEqual(await tools.call("phone.status", {}), status);
    assert.deepEqual(
      await jsonFile(join(dataRoot, "phone-native-owner.json")),
      owner,
    );
    assert.equal(running.journal.status().operations, 1);
    await checkHealth(config);
    await running.close();
    running = null;
    const db = new DatabaseSync(join(dataRoot, "phone-commands.sqlite"), {
      readOnly: true,
    });
    let originalOperationId: string, originalOperation: unknown;
    try {
      assert.equal(
        db.prepare("SELECT value FROM meta WHERE key='epoch'").get()?.["value"],
        "",
      );
      const original = db
        .prepare(
          "SELECT operation_id,phase,value FROM commands WHERE epoch=? AND method='configure'",
        )
        .get(status.epoch);
      assert.equal(original?.["phase"], "result");
      originalOperationId = String(original!["operation_id"]);
      originalOperation = JSON.parse(String(original!["value"]));
    } finally {
      db.close();
    }
    running = await startPhoneBridge(path);
    await running.service.waitReady();
    const replacement = (await tools.call("phone.status", {})) as {
      epoch: string;
      busy: boolean;
    };
    assert.notEqual(replacement.epoch, status.epoch);
    assert.equal(replacement.busy, false);
    assert.equal(running.journal.status().operations, 1);
    assert.equal(running.journal.status().calls, 0);
    assert.deepEqual(
      running.journal.get(originalOperationId),
      originalOperation,
    );
    assert.equal(running.journal.archiveStatus().records, 2); // Original configure receipt and retired epoch.
    assert.equal(
      (
        await jsonFile<{ epoch: string }>(
          join(dataRoot, "phone-native-owner.json"),
        )
      ).epoch,
      replacement.epoch,
    );
    await checkHealth(config);
  },
);
