import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join, relative } from "node:path";
import { tmpdir } from "node:os";
import { connect, createServer, type Socket } from "node:net";
import { randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { digest } from "../packages/contracts/src/canonical.js";
import { HiveServer } from "../services/hive/src/server.js";
import { HiveClient, serviceTools } from "../packages/sdk/src/client.js";
import { atomicJson, jsonFile } from "../packages/host-runtime/src/config.js";
import { fileHash } from "../packages/host-runtime/src/artifact.js";
import { checkHealth } from "../packages/host-runtime/src/health.js";
import { startNativePhone, phoneArtifactRoot } from "./fixtures/phone-service.js";
import { phoneRegistry } from "./fixtures/phone-protocol.js";
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
      artifactRoot = phoneArtifactRoot();
    const build = JSON.parse(
      await readFile(join(artifactRoot, "dist/build-info.json"), "utf8"),
    ) as { version: string; buildId: string };
    const executable = "dist/native/phone/Ivy.PhoneRuntime.exe",
      executableHash = await fileHash(join(artifactRoot, executable));
    const listener = createServer();
    await new Promise<void>((done) => listener.listen(0, "127.0.0.1", done));
    const port = (listener.address() as { port: number }).port;
    await new Promise<void>((done) => listener.close(() => done()));
    const proxied = new Set<Socket>();
    const proxy = createServer(source => {
      const target=connect(port,"127.0.0.1");
      proxied.add(source);proxied.add(target);
      const close=()=>{proxied.delete(source);proxied.delete(target);source.destroy();target.destroy();};
      source.on("error",close);target.on("error",close);
      source.on("close",close);target.on("close",close);
      source.pipe(target).pipe(source);
    });
    await new Promise<void>(done=>proxy.listen(0,"127.0.0.1",done));
    const base = `http://127.0.0.1:${(proxy.address() as {port:number}).port}/ivy`,
      dataRoot = join(root, "phone");
    function journalCounts(epoch: string) {
      const db=new DatabaseSync(join(dataRoot,"phone-commands.sqlite"),{readOnly:true});
      try {
        const count=(sql:string)=>Number(db.prepare(sql).get()?.["n"]);
        return {operations:Number(db.prepare("SELECT count(*) n FROM commands WHERE epoch=?").get(epoch)?.["n"]),
          calls:count("SELECT count(*) n FROM active_calls"),archive:count("SELECT count(*) n FROM receipt_archive")};
      } finally {db.close();}
    }
    function persistedOperation(id:string) {
      const db=new DatabaseSync(join(dataRoot,"phone-commands.sqlite"),{readOnly:true});
      try {return JSON.parse(String(db.prepare("SELECT value FROM receipt_archive WHERE kind='command' AND key=?").get(id)?.["value"]));}
      finally {db.close();}
    }
    const hiveOptions = {
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
    };
    const hive = new HiveServer(hiveOptions);
    let running: Awaited<ReturnType<typeof startNativePhone>> | null = null;
    t.after(async () => {
      try {
        await running?.close();
      } finally {
        await hive.close();
        for(const socket of proxied)socket.destroy();
        await new Promise<void>((done,reject)=>proxy.close(error=>error?reject(error):done()));
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
      workRoot: join(root,"work"),
      logsRoot: join(root,"logs"),
      version: build.version,
      buildId: build.buildId,
      credential: "phone-owner-fixture-token",
      settings: {
        native: { executable, executableHash },
        binding: { address: "127.0.0.1", port: 0, transport: "udp" },
        codecs: { preferences: ["G722"], packetMs: 20 },
        registration: null,
        credentialsPath: null,
        codexVoice: null,
        policy: { recipients: [], incoming: [] },
        incomingPrincipalId: null,
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
        ringSeconds: 30,
        pollMs: 500,
      },
    };
    const path = join(root, "phone.json");
    await atomicJson(path, config);
    running = await startNativePhone(path);
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
    const waitPublished = (ready=true) => until(async()=>{
      const result=await admin.request("serviceNodes.list",{serviceName:"phone-bridge"});
      const node=result.items.find(node=>node.serviceNodeId==="phone");
      return ready ? !!node?.connected && node.synced && node.ready && !node.stale : !node?.connected;
    },30000);
    await waitPublished();
    const status = (await tools.call("phone.status", {})) as {
      epoch: string;
      busy: boolean;
      recipients: string[];
      call: unknown;
      incoming: unknown;
    };
    assert.match(status.epoch, /^[0-9a-f-]{36}$/);
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
      phoneRegistry().namespaces[0]!.tools.length,
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
    assert.deepEqual((await tools.call("phone.calls", {})) as {calls:unknown[]}, {calls:[]});
    const owner = await jsonFile<{
      epoch: string;
      pid: number;
      creationFileTime: string;
    }>(join(dataRoot, "phone-native-owner.json"));
    assert.equal(owner.epoch, status.epoch);
    assert.ok(owner.pid > 0);
    await checkHealth(config);
    const generation = (await running.health()).generation;
    // Drop the transport without replacing Hive or its generation counter.
    for(const socket of proxied)socket.destroy();
    await running.waitLocalReady(generation);
    await waitPublished();
    assert.deepEqual(await tools.call("phone.status", {}), status);
    assert.deepEqual(
      await jsonFile(join(dataRoot, "phone-native-owner.json")),
      owner,
    );
    await checkHealth(config);
    await running.close();
    running = null;
    await waitPublished(false);
    assert.equal(journalCounts(status.epoch).operations, 1);
    assert.equal(journalCounts(status.epoch).calls, 0);
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
    running = await startNativePhone(path);
    await waitPublished();
    const replacement = (await tools.call("phone.status", {})) as {
      epoch: string;
      busy: boolean;
    };
    assert.notEqual(replacement.epoch, status.epoch);
    assert.equal(replacement.busy, false);
    assert.deepEqual((await tools.call("phone.calls", {})) as {calls:unknown[]}, {calls:[]});
    assert.equal(
      (
        await jsonFile<{ epoch: string }>(
          join(dataRoot, "phone-native-owner.json"),
        )
      ).epoch,
      replacement.epoch,
    );
    await checkHealth(config);
    await running.close();
    running = null;
    await waitPublished(false);
    assert.equal(journalCounts(replacement.epoch).operations, 1);
    assert.equal(journalCounts(replacement.epoch).calls, 0);
    assert.deepEqual(
      persistedOperation(originalOperationId),
      originalOperation,
    );
    assert.equal(journalCounts(replacement.epoch).archive, 2); // Original configure receipt and retired epoch.
  },
);
