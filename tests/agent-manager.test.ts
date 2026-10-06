import test from "node:test";
import assert from "node:assert/strict";
import {
  readFileSync,
  mkdtempSync,
  rmSync,
  existsSync,
  mkdirSync,
  writeFileSync,
} from "node:fs";
import { join, relative, resolve } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { PassThrough } from "node:stream";
import { createServer } from "node:net";
import { LocalAgentState } from "../services/agent-manager/src/hive-state.js";
import { NativeRpc } from "../services/agent-manager/src/rpc.js";
import { startAgentManager } from "../services/agent-manager/src/main.js";
import type { NativeLauncher } from "../services/agent-manager/src/main.js";
import { HiveServer } from "../services/hive/src/server.js";
import { HiveClient, callBound, discover } from "../packages/sdk/src/client.js";
import { readNativeContract } from "../packages/sdk/src/native.js";
import { atomicJson } from "../packages/host-runtime/src/config.js";
import { HealthFile } from "../packages/host-runtime/src/health.js";
import { digest, hashJson } from "../packages/contracts/src/canonical.js";
import { operationId as makeOperationId } from "../packages/contracts/src/operation-id.js";
import { validateAgent } from "../packages/contracts/src/validation.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import { until } from "./fixtures/host.js";
import type { Agent, Host, Wire } from "../packages/contracts/src/generated.js";
import { applyAgentRuntimeReset } from "../services/agent-manager/src/runtime-reset.js";

test("AgentManager consumes a reset marker idempotently against its explicit Codex home", async (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-agent-reset-")),
    dataRoot = join(root, "data"),
    home = join(root, "configured-codex-home");
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(home, "sessions"), { recursive: true });
  mkdirSync(dataRoot, { recursive: true });
  writeFileSync(join(home, "sessions", "task.jsonl"), "transient");
  writeFileSync(join(home, "config.toml"), "preserved");
  await atomicJson(join(dataRoot, "runtime-reset.json"), {
    schemaVersion: 1,
    resetId: "reset-one",
    runtimeEpoch: "epoch-one",
    completedAt: new Date().toISOString(),
  });
  const settings = { codexHome: home } as Agent.Settings;
  await applyAgentRuntimeReset(dataRoot, settings);
  await applyAgentRuntimeReset(dataRoot, settings);
  assert.equal(existsSync(join(home, "sessions")), false);
  assert.equal(readFileSync(join(home, "config.toml"), "utf8"), "preserved");
  const applied = JSON.parse(
    readFileSync(join(dataRoot, "runtime-reset-applied.json"), "utf8"),
  ) as { resetId: string; runtimeEpoch: string };
  assert.deepEqual(
    { resetId: applied.resetId, runtimeEpoch: applied.runtimeEpoch },
    { resetId: "reset-one", runtimeEpoch: "epoch-one" },
  );
});

for (const version of ["0.154.0"] as const)
  test(
    `AgentManager ${version} with a native protocol fixture preserves real Hive caller outcomes across reconnect, answer resolution and owner restart`,
    { timeout: 45_000 },
    async (t) => {
      const root = mkdtempSync(join(tmpdir(), "ivy-agent-service-"));
      const reservation = createServer();
      await new Promise<void>((resolve) =>
        reservation.listen(0, "127.0.0.1", resolve),
      );
      const port = (reservation.address() as { port: number }).port;
      await new Promise<void>((resolve) => reservation.close(() => resolve()));
      const base = `http://127.0.0.1:${port}/ivy`,
        build = JSON.parse(readFileSync("dist/build-info.json", "utf8")) as {
          buildId: string;
          version: string;
        };
      const full = JSON.parse(
        readFileSync(`specs/native/codex-${version}/catalog.json`, "utf8"),
      ) as Agent.Catalog;
      // This is explicitly a protocol simulation; separate tests admit the complete supported catalog.
      const catalog = {
        ...full,
        clientRequests: full.clientRequests.filter((value) =>
          [
            "thread/list",
            "thread/loaded/list",
            "thread/read",
            "thread/turns/list",
            "thread/items/list",
            "project/list",
            "account/logout",
            "account/rateLimits/read",
          ].includes(value.method),
        ),
      };
      const thread = {
        cliVersion: full.version,
        createdAt: 1788690000,
        cwd: "/fixture",
        ephemeral: false,
        id: "read-thread",
        modelProvider: "openai",
        preview: "Read fixture",
        projectId: null,
        sessionId: "read-thread",
        source: "appServer",
        status: { type: "idle" },
        turns: [],
        updatedAt: 1788690000,
        canAcceptDirectInput: true,
        name: "Fixture",
      };
      const server = new HiveServer({
        filename: join(root, "hive.sqlite"),
        publicBaseUrl: base,
        version: "test",
        buildId: digest("agent-hive"),
        listenPort: port,
        credentials: [
          { principalId: "user", digest: digest("user-token") },
          { principalId: "other", digest: digest("other-token") },
          { principalId: "service", digest: digest("service-token") },
          {
            principalId: "ivy-native-clock",
            digest: digest("reserved-fixture-token"),
          },
        ],
        callTimeoutMs: 3000,
      });
      await server.start();
      const settings: Agent.Settings = {
        nativeExecutable: process.execPath,
        nativeHome: join(root, "data", "native"),
        nativeVersion: full.version,
        nativeExecutableHash: full.nativeExecutableHash,
        // This scenario retains a native request, its unresolved clock answer and a refused caller
        // operation concurrently. The dedicated clock capacity test checks refusal below that budget.
        limits: {
          maxOperations: 100,
          maxJournalBytes: 128 * 1024 * 1024,
          maxPendingInputs: 16,
          maxNotificationBytes: 1048576,
        },
      };
      const configuredSettings: Partial<Agent.Settings> = { ...settings };
      const config: Host.InstanceConfig = {
        schemaVersion: 1,
        componentId: "agent-manager",
        instanceId: "agent",
        serviceNodeId: "agent-node",
        hostId: "fixture-host",
        publicBaseUrl: base,
        artifactRoot: resolve("."),
        dataRoot: join(root, "data"),
        buildId: build.buildId,
        version: build.version,
        credential: "service-token",
        settings: configuredSettings,
      };
      const path = join(root, "config.json");
      await atomicJson(path, config);
      const owners: {
        rpc: NativeRpc;
        epoch: string;
        sent: Record<string, Wire.Json>[];
        emit: (value: unknown) => void;
        stopped: boolean;
      }[] = [];
      let holdInventory = true;
      const heldInventory: (() => void)[] = [];
      const launcher: NativeLauncher = async (options) => {
        assert.equal(
          options.hiveCredential,
          "service-token",
          "Managed Hive access uses the configured service principal, not the invoking user or inherited environment.",
        );
        const epoch = randomUUID(),
          input = new PassThrough(),
          output = new PassThrough(),
          sent: Record<string, Wire.Json>[] = [];
        options.beginEpoch(epoch);
        const rpc = new NativeRpc(catalog, input, output, {
          onClose: (code) => options.onClose(epoch, code),
          onRequest: (value) => options.onRequest(epoch, value),
          onNotification: (value) => options.onNotification(epoch, value),
        });
        const owner = {
          rpc,
          epoch,
          sent,
          emit: (value: unknown) => {
            output.write(JSON.stringify(value) + "\n");
          },
          stopped: false,
        };
        owners.push(owner);
        input.on("data", (bytes: Buffer) => {
          const frame = JSON.parse(bytes.toString("utf8")) as Record<
            string,
            Wire.Json
          >;
          sent.push(frame);
          if (
            typeof frame["method"] === "string" &&
            ["thread/list", "thread/loaded/list", "project/list"].includes(
              frame["method"],
            )
          ) {
            const reply = () =>
              owner.emit({
                id: frame["id"],
                result: { data: [], nextCursor: null },
              });
            if (holdInventory) heldInventory.push(reply);
            else reply();
          }
          const params = frame["params"] as
            Record<string, Wire.Json> | undefined;
          if (params?.["threadId"] === "read-thread") {
            if (frame["method"] === "thread/read")
              owner.emit({ id: frame["id"], result: { thread } });
            if (
              frame["method"] === "thread/turns/list" ||
              frame["method"] === "thread/items/list"
            )
              owner.emit({
                id: frame["id"],
                result: { data: [], nextCursor: null },
              });
          }
          if (params?.["threadId"] === "missing-thread")
            owner.emit({
              id: frame["id"],
              error: {
                code: -32000,
                message: "Missing original thread",
                data: { threadId: params["threadId"] },
              },
            });
          if (frame["id"] === "startup-clock" && frame["result"])
            owner.emit({
              method: "serverRequest/resolved",
              params: {
                requestId: "startup-clock",
                threadId: "fixture-thread",
              },
            });
        });
        owner.emit({
          id: "startup-clock",
          method: "currentTime/read",
          params: { threadId: "fixture-thread" },
        });
        return {
          rpc,
          epoch,
          catalog,
          initialized: { codexHome: settings.nativeHome! },
          launcherPid: null,
          close: async () => {
            owner.stopped = true;
            rpc.close("fixture_stop");
            input.destroy();
            output.destroy();
          },
        };
      };
      const handles: Awaited<ReturnType<typeof startAgentManager>>[] = [];
      t.after(async () => {
        holdInventory = false;
        for (const reply of heldInventory.splice(0)) reply();
        for (const manager of handles) await manager.close();
        await server.close();
        assert.ok(relative(tmpdir(), root).startsWith("ivy-agent-service-"));
        rmSync(root, { recursive: true, force: true });
      });
      const manager = await startAgentManager(path, launcher);
      handles.push(manager);
      await manager.service.waitReady({ timeoutMs: 20_000 });
      const first = owners[0]!,
        client = new HiveClient(base, { credential: "user-token" }),
        other = new HiveClient(base, { credential: "other-token" });
      const runtimeEpoch = (await client.request("system.status", {}))
        .runtimeEpoch;
      const operationIds = new Map<string, string>();
      const oid = (value: string) =>
        operationIds.get(value) ??
        (() => {
          const id = makeOperationId(runtimeEpoch, Date.now(), value);
          operationIds.set(value, id);
          return id;
        })();
      assert.equal(
        first.sent.filter((value) => value["id"] === "startup-clock").length,
        1,
        "A clock observed during startup is answered when its exact owned RPC becomes available.",
      );
      const binding = await discover(client, "codex.account/logout", {
        serviceNodeId: "agent-node",
      });
      const tool = async (name: string, args: Wire.Json, id?: string) => {
        if (
          args &&
          typeof args === "object" &&
          !Array.isArray(args) &&
          typeof args["operationId"] === "string" &&
          !args["operationId"].includes(":")
        )
          args["operationId"] = oid(args["operationId"]);
        return callBound(
          client,
          await discover(client, name, { serviceNodeId: "agent-node" }),
          args,
          id && !id.includes(":") ? oid(id) : id,
        );
      };
      const operation = async (operationId: string) =>
        (await tool("agent.operation", { operationId })) as Agent.Operation;
      assert.equal(
        ((await tool("agent.status", {})) as Agent.Status).state,
        "ready",
        "Inventory must not gate native interactions.",
      );
      // Exercise the real service handler while both durable operation storage and background
      // inventory are unavailable. Interactive execution must touch neither dependency.
      const interactiveBinding = await discover(client, "codex.thread/read", {
        serviceNodeId: "agent-node",
      });
      const priorRead = LocalAgentState.prototype.read,
        priorWrite = LocalAgentState.prototype.write;
      LocalAgentState.prototype.read = async () => {
        throw Error("Interactive path read local workflow state");
      };
      LocalAgentState.prototype.write = async () => {
        throw Error("Interactive path wrote local workflow state");
      };
      try {
        const request = {
          operationId: "fast-interaction",
          nativeVersion: version,
          method: "thread/read",
          params: { threadId: "read-thread", includeTurns: false },
          expectedDefinitionHash: interactiveBinding.definitionHash,
        };
        const before = first.sent.length;
        const reply = (await tool(
          "agent.interact",
          request,
          request.operationId,
        )) as Agent.Interaction;
        assert.deepEqual(reply.reply, { result: { thread } });
        assert.deepEqual(
          await tool("agent.interaction", { operationId: request.operationId }),
          reply,
        );
        assert.deepEqual(
          await tool("agent.interact", request, request.operationId),
          reply,
        );
        assert.equal(
          first.sent.length - before,
          1,
          "One native dispatch, including lookup and duplicate invocation.",
        );
        await assert.rejects(
          callBound(
            other,
            await discover(other, "agent.interaction", {
              serviceNodeId: "agent-node",
            }),
            { operationId: request.operationId },
          ),
          { code: "interaction_expired" },
        );
      } finally {
        LocalAgentState.prototype.read = priorRead;
        LocalAgentState.prototype.write = priorWrite;
      }
      assert.ok(heldInventory.length);
      holdInventory = false;
      for (const reply of heldInventory.splice(0)) reply();
      await until(async () => {
        try {
          await tool("agent.projects", {});
          return true;
        } catch {
          return false;
        }
      });
      const directRead = await discover(client, "codex.thread/list", {
        serviceNodeId: "agent-node",
      });
      assert.equal(
        directRead.definition.annotations?.readOnlyHint,
        true,
        "Native task listing is explicitly read-only.",
      );
      const operationsBeforeDirectRead = (
        (await tool("agent.status", {})) as Agent.Status
      ).operations;
      const listed = await callBound(client, directRead, {
        archived: false,
        limit: 10,
        sourceKinds: [],
        modelProviders: [],
        sortKey: "recency_at",
        sortDirection: "desc",
        useStateDbOnly: true,
      });
      assert.deepEqual(
        listed,
        { data: [], nextCursor: null },
        "A native task listing returns its unchanged native page.",
      );
      assert.deepEqual(
        ((await tool("agent.status", {})) as Agent.Status).operations,
        operationsBeforeDirectRead,
        "A read-only native task listing requires no operation identity or durable receipt.",
      );
      const reserved = new HiveClient(base, {
        credential: "reserved-fixture-token",
      });
      await assert.rejects(
        callBound(
          reserved,
          await discover(reserved, "agent.operation", {
            serviceNodeId: "agent-node",
          }),
          { operationId: "cannot-adopt-local-clock" },
        ),
        (error: unknown) =>
          error instanceof IvyError &&
          error.code === "native_reserved_principal",
      );
      const frameLimits = (await tool(
        "agent.frameLimits",
        {},
      )) as Agent.FrameLimits;
      validateAgent("FrameLimits", frameLimits);
      assert.deepEqual(frameLimits, {
        serviceNodeId: "agent-node",
        nativeVersion: catalog.version,
        nativeExecutableHash: catalog.nativeExecutableHash,
        catalogHash: hashJson(catalog),
        epoch: first.epoch,
        requestFrameBytes: 6291456,
        receivedFrameBytes: 25165824,
        answerFrameBytes: 4194304,
        managementFrameBytes: 33554432,
      });
      const catalogFrames = first.sent.length,
        catalogOperations = ((await tool("agent.status", {})) as Agent.Status)
          .operations;
      const selectedContract = await readNativeContract(client, {
        serviceNodeId: "agent-node",
        hostId: "fixture-host",
        nativeVersion: catalog.version,
        nativeExecutableHash: catalog.nativeExecutableHash,
        catalogHash: hashJson(catalog),
      });
      assert.equal(selectedContract.epoch, first.epoch);
      assert.deepEqual(selectedContract.contract.catalog, catalog);
      const selectedRead = await discover(client, "codex.thread/read", {
        serviceNodeId: "agent-node",
      });
      selectedContract.contract.verifyDefinition(
        "thread/read",
        selectedRead.definition,
        selectedRead.definitionHash,
      );
      assert.equal(
        first.sent.length,
        catalogFrames,
        "Catalog discovery is management-only and does not send native requests.",
      );
      assert.deepEqual(
        ((await tool("agent.status", {})) as Agent.Status).operations,
        catalogOperations,
        "Catalog reads consume no native operation capacity.",
      );
      await assert.rejects(operation("never-submitted"), (error: unknown) => {
        assert.ok(error instanceof IvyError);
        assert.equal(error.code, "not_found");
        assert.deepEqual(error.details, {
          kind: "agent_operation_absent",
          operationId: oid("never-submitted"),
          serviceNodeId: "agent-node",
          epoch: first.epoch,
        });
        return true;
      });
      assert.equal(
        first.sent.some(
          (value) => value["method"] === "account/rateLimits/read",
        ),
        false,
        "The compatibility probe must never reach native execution.",
      );
      const readBinding = await discover(client, "agent.read", {
        serviceNodeId: "agent-node",
      });
      const readStatus = ((await tool("agent.status", {})) as Agent.Status)
        .operations;
      const readRequest: Agent.ReadInput = {
        nativeVersion: version,
        method: "thread/read",
        params: { threadId: thread.id, includeTurns: false },
      };
      const observation = (await callBound(
        client,
        readBinding,
        readRequest,
      )) as Agent.ReadObservation;
      validateAgent("ReadObservation", observation);
      assert.equal(observation.callerPrincipalId, "user");
      assert.equal(observation.serviceNodeId, "agent-node");
      assert.equal(observation.epoch, first.epoch);
      assert.equal(observation.nativeExecutableHash, full.nativeExecutableHash);
      assert.equal(observation.catalogHash, hashJson(catalog));
      assert.equal(observation.nativeVersion, full.version);
      assert.deepEqual(observation.reply, { result: { thread } });
      assert.deepEqual(observation.params, readRequest.params);
      assert.equal(
        observation.requestHash,
        hashJson({ method: readRequest.method, params: readRequest.params }),
      );
      assert.equal(
        observation.requestId,
        first.sent.findLast((value) => value["method"] === "thread/read")![
          "id"
        ],
      );
      assert.ok(Number.isFinite(Date.parse(observation.observedAt)));
      const secondRead = (await callBound(
        other,
        readBinding,
        readRequest,
      )) as Agent.ReadObservation;
      assert.equal(secondRead.callerPrincipalId, "other");
      assert.notEqual(secondRead.observationId, observation.observationId);
      assert.notEqual(secondRead.requestId, observation.requestId);
      for (let i = 0; i < 101; i++) {
        const method = i % 2 ? "thread/turns/list" : "thread/items/list";
        const params = {
          threadId: thread.id,
          limit: 50,
          cursor: null,
          sortDirection: "asc",
          ...(i % 2 ? { itemsView: "notLoaded" } : { turnId: "turn" }),
        };
        const page = (await callBound(client, readBinding, {
          nativeVersion: full.version,
          method,
          params,
        })) as Agent.ReadObservation;
        assert.deepEqual(page.params, params);
        assert.deepEqual(page.reply, {
          result: { data: [], nextCursor: null },
        });
      }
      const errorRead = (await callBound(client, readBinding, {
        ...readRequest,
        params: { threadId: "missing-thread" },
      })) as Agent.ReadObservation;
      assert.deepEqual(errorRead.reply, {
        error: {
          code: -32000,
          message: "Missing original thread",
          data: { threadId: "missing-thread" },
        },
      });
      const foregroundFrames = () =>
        first.sent.filter(
          (frame) =>
            !["thread/list", "thread/loaded/list", "project/list"].includes(
              String(frame["method"]),
            ),
        );
      const countBeforeInvalid = foregroundFrames().length;
      await assert.rejects(
        callBound(client, readBinding, {
          ...readRequest,
          nativeVersion: "0.0.0",
        }),
        (error: unknown) =>
          error instanceof IvyError && error.code === "native_version_mismatch",
      );
      for (const change of [
        { method: "turn/start" },
        { nativeVersion: "unknown" },
      ])
        await assert.rejects(
          callBound(client, readBinding, { ...readRequest, ...change }),
        );
      assert.equal(
        foregroundFrames().length,
        countBeforeInvalid,
        "Refused reads never reach native dispatch; independent periodic inventory may continue.",
      );
      assert.deepEqual(
        ((await tool("agent.status", {})) as Agent.Status).operations,
        readStatus,
        "More reads than the operation limit consume no journal capacity.",
      );
      await assert.rejects(
        operation(observation.observationId),
        (error: unknown) =>
          error instanceof IvyError && error.code === "not_found",
      );
      const prevention: Agent.PreventInput = {
        operationId: "prevent-before-dispatch",
        nativeVersion: version,
        method: "account/logout",
        params: null,
        expectedDefinitionHash: binding.definitionHash,
      };
      const prevented = (await tool(
        "agent.prevent",
        prevention,
        prevention.operationId,
      )) as Agent.Operation;
      assert.equal(prevented.phase, "failed");
      assert.equal(prevented.code, "request_prevented");
      assert.equal(prevented.requestId, null);
      assert.equal(prevented.epoch, null);
      assert.equal(prevented.reply, null);
      assert.deepEqual(await operation(prevention.operationId), prevented);
      await assert.rejects(
        callBound(client, binding, null, prevention.operationId),
        (error: unknown) =>
          error instanceof IvyError &&
          error.code === "request_prevented" &&
          error.outcome === "not_executed",
      );
      assert.equal(
        first.sent.some((value) => value["method"] === "account/logout"),
        false,
      );
      for (const change of [
        { nativeVersion: "0.0.0" },
        { expectedDefinitionHash: digest("wrong") },
        { method: "agent.answer" },
      ])
        await assert.rejects(
          tool("agent.prevent", {
            ...prevention,
            ...change,
            operationId: "invalid-prevention",
          }),
        );
      await assert.rejects(tool("agent.prevent", prevention, "wrong-outer-id"));
      await assert.rejects(
        operation("invalid-prevention"),
        (error: unknown) =>
          error instanceof IvyError && error.code === "not_found",
      );
      await assert.rejects(
        callBound(client, binding, null),
        (error: unknown) =>
          error instanceof IvyError && error.code === "operation_id_required",
      );
      const invokeBinding = await discover(client, "agent.invoke", {
        serviceNodeId: "agent-node",
      });
      const callerLost = oid("caller-lost");
      const controller = new AbortController(),
        pending = callBound(
          client,
          invokeBinding,
          { ...prevention, operationId: callerLost },
          callerLost,
          { signal: controller.signal },
        );
      void pending.catch(() => undefined);
      await until(() =>
        first.sent.some((value) => value["method"] === "account/logout"),
      );
      const original = first.sent.find(
        (value) => value["method"] === "account/logout",
      )!;
      const alreadyDispatched = await operation("caller-lost");
      assert.deepEqual(
        await tool("agent.prevent", {
          ...prevention,
          operationId: "caller-lost",
        }),
        alreadyDispatched,
      );
      await assert.rejects(
        callBound(client, binding, null, callerLost),
        (error: unknown) =>
          error instanceof IvyError &&
          error.code === "native_operation_pending" &&
          error.outcome === "unknown",
      );
      controller.abort();
      await assert.rejects(pending);
      const generation = manager.service.connection.generation;
      manager.service.connection.close();
      first.emit({
        id: original["id"],
        result: { original: ["retained", null, false] },
      });
      await manager.service.waitReady({ timeoutMs: 20_000 });
      assert.ok(manager.service.connection.generation > generation);
      assert.equal(owners.length, 1);
      assert.deepEqual(await callBound(client, binding, null, callerLost), {
        original: ["retained", null, false],
      });
      assert.equal(
        first.sent.filter((value) => value["method"] === "account/logout")
          .length,
        1,
      );
      const recorded = await operation("caller-lost");
      assert.equal(recorded.phase, "succeeded");
      assert.equal(recorded.requestId, original["id"]);
      assert.deepEqual(
        await tool("agent.prevent", {
          ...prevention,
          operationId: "caller-lost",
        }),
        recorded,
      );
      assert.deepEqual(
        await tool(
          "agent.invoke",
          { ...prevention, operationId: "caller-lost" },
          "caller-lost",
        ),
        recorded,
      );
      await assert.rejects(
        callBound(
          other,
          await discover(other, "agent.operation", {
            serviceNodeId: "agent-node",
          }),
          { operationId: "caller-lost" },
        ),
        (error: unknown) =>
          error instanceof IvyError && error.code === "not_found",
      );
      const questionParams = {
        threadId: "fixture-thread",
        turnId: "fixture-turn",
        itemId: "fixture-question",
        isBlocking: true,
        questions: [
          {
            id: "choice",
            header: "Choice",
            question: "Continue this fixture?",
            options: [
              {
                label: "Continue",
                description: "Continue the isolated protocol fixture.",
              },
            ],
          },
        ],
      };
      first.emit({
        id: -9,
        method: "item/tool/requestUserInput",
        params: questionParams,
      });
      const input = ((await tool("agent.inputs", {})) as Agent.PendingInputPage)
        .items[0]!;
      assert.equal(input.identity.requestId, -9);
      const responseDefinition = (await tool("agent.inputDefinition", {
        identity: input.identity,
      })) as Agent.InputDefinition;
      assert.equal(responseDefinition.method, input.method);
      assert.equal(responseDefinition.nativeVersion, full.version);
      assert.deepEqual(
        responseDefinition.responseSchema,
        catalog.serverRequests.find((value) => value.method === input.method)!
          .outputSchema,
      );
      await assert.rejects(
        tool("agent.inputDefinition", {
          identity: { ...input.identity, serviceNodeId: "another-node" },
        }),
      );
      const answer = {
        operationId: "answer-once",
        identity: input.identity,
        reply: { result: { answers: { choice: { answers: ["Continue"] } } } },
      };
      assert.equal(
        (
          (await tool(
            "agent.answer",
            answer,
            answer.operationId,
          )) as Agent.Operation
        ).phase,
        "dispatched",
      );
      assert.equal(
        (
          (await tool(
            "agent.answer",
            answer,
            answer.operationId,
          )) as Agent.Operation
        ).phase,
        "dispatched",
      );
      assert.equal(first.sent.filter((value) => value["id"] === -9).length, 1);
      first.emit({
        method: "serverRequest/resolved",
        params: { requestId: -9, threadId: "fixture-thread" },
      });
      assert.equal((await operation(answer.operationId)).phase, "succeeded");
      assert.equal(
        ((await tool("agent.inputs", {})) as Agent.PendingInputPage).items
          .length,
        0,
      );
      const events = (await tool("agent.notifications", {
        afterSequence: 0,
      })) as Agent.NotificationPage;
      assert.equal(events.items.at(-1)?.method, "serverRequest/resolved");
      assert.equal(events.gap, false);
      const nativeLost = oid("native-lost");
      const crashing = callBound(client, binding, null, nativeLost);
      void crashing.catch(() => undefined);
      await until(
        () =>
          first.sent.filter((value) => value["method"] === "account/logout")
            .length === 2,
      );
      const lostRead = callBound(client, readBinding, {
        ...readRequest,
        params: { threadId: "waiting-read" },
      });
      void lostRead.catch(() => undefined);
      await until(() =>
        first.sent.some(
          (value) =>
            (value["params"] as Record<string, Wire.Json> | undefined)?.[
              "threadId"
            ] === "waiting-read",
        ),
      );
      first.emit({
        id: "-9",
        method: "item/tool/requestUserInput",
        params: questionParams,
      });
      first.rpc.close("fixture_native_crash");
      await assert.rejects(crashing);
      await assert.rejects(lostRead);
      assert.equal(await manager.closed, "fixture_native_crash");
      assert.equal(first.stopped, true);
      assert.equal(
        existsSync(join(config.dataRoot, "journal/agent-runtime.sqlite")),
        true,
      );
      const replacement = await startAgentManager(path, launcher);
      handles.push(replacement);
      await replacement.service.waitReady({ timeoutMs: 20_000 });
      assert.equal(owners.length, 2);
      assert.notEqual(owners[1]!.epoch, first.epoch);
      assert.equal((await operation("native-lost")).phase, "outcome_unknown");
      assert.deepEqual(await tool("agent.prevent", prevention), prevented);
      assert.deepEqual(
        await tool("agent.prevent", {
          ...prevention,
          operationId: "native-lost",
        }),
        await operation("native-lost"),
      );
      await assert.rejects(
        callBound(client, binding, null, prevention.operationId),
        (error: unknown) =>
          error instanceof IvyError && error.code === "request_prevented",
      );
      const replacementRead = (await callBound(
        client,
        readBinding,
        readRequest,
      )) as Agent.ReadObservation;
      assert.equal(replacementRead.epoch, owners[1]!.epoch);
      assert.notEqual(replacementRead.epoch, observation.epoch);
      assert.deepEqual(replacementRead.reply, observation.reply);
      await assert.rejects(
        callBound(client, binding, null, nativeLost),
        (error: unknown) =>
          error instanceof IvyError && error.outcome === "unknown",
      );
      assert.equal(
        owners[1]!.sent.some((value) => value["method"] === "account/logout"),
        false,
      );
      assert.equal(
        (
          (await tool("agent.inputs", {
            includeExpired: true,
          })) as Agent.PendingInputPage
        ).items.some((value) => value.identity.requestId === "-9"),
        false,
      );
      const expired = {
        identity: {
          serviceNodeId: "agent-node",
          epoch: first.epoch,
          requestId: "-9",
        },
      };
      await assert.rejects(
        tool("agent.inputDefinition", { identity: expired.identity }),
      );
      const staleAnswer = (await tool("agent.answer", {
        operationId: "stale-epoch-answer",
        identity: expired.identity,
        reply: answer.reply,
      })) as Agent.Operation;
      assert.equal(staleAnswer.phase, "failed");
      assert.equal(staleAnswer.code, "not_found");
      assert.equal(
        owners[1]!.sent.some((value) => value["id"] === "-9"),
        false,
      );
      const status = (await tool("agent.status", {})) as Agent.Status;
      assert.equal(status.epoch, owners[1]!.epoch);
      assert.equal(status.state, "ready");
      assert.equal(JSON.stringify(status).includes("service-token"), false);
      const hiveState = new LocalAgentState(
        join(config.dataRoot, "journal/fixture.sqlite"),
        "agent-node",
        settings.limits,
      );
      const large = { payload: "x".repeat(6 * 1024 * 1024) };
      await hiveState.write("fixture", "large", large);
      hiveState.close();
      const reopenedState = new LocalAgentState(
        join(config.dataRoot, "journal/fixture.sqlite"),
        "agent-node",
        settings.limits,
      );
      assert.deepEqual(
        (await reopenedState.read("fixture", "large"))!.value,
        large,
      );
      reopenedState.close();
      const second = owners[1]!;
      const healthWrite = HealthFile.prototype.write;
      let healthFailure = false;
      const transientHealth = t.mock.method(
        HealthFile.prototype,
        "write",
        async function (
          this: HealthFile,
          ready: boolean,
          details?: string,
          generation?: number | null,
        ) {
          if (!ready && !healthFailure) {
            healthFailure = true;
            throw Object.assign(new Error("Temporary health-file lock"), {
              code: "EBUSY",
            });
          }
          return healthWrite.call(this, ready, details, generation);
        },
      );
      const disconnected = replacement.service.connection;
      disconnected.close();
      second.emit({
        id: "local-clock",
        method: "currentTime/read",
        params: { threadId: "fixture-thread" },
      });
      const clockFrame = second.sent.find(
        (value) => value["id"] === "local-clock",
      )!;
      assert.ok(clockFrame);
      const clockTime = (clockFrame["result"] as { currentTimeAt: number })
        .currentTimeAt;
      assert.ok(
        Math.abs(Math.floor(Date.now() / 1000) - clockTime) <= 1,
        "The owning service answers the real clock even without a ready Hive connection.",
      );
      await replacement.service.waitReady({ timeoutMs: 20_000 });
      assert.equal(healthFailure, true);
      assert.equal(
        second.stopped,
        false,
        "A transient health-file failure does not replace the native owner.",
      );
      transientHealth.mock.restore();
      const clockInput = (
        (await tool("agent.inputs", {})) as Agent.PendingInputPage
      ).items.find((value) => value.identity.requestId === "local-clock")!;
      assert.equal(clockInput.state, "answering");
      assert.equal(clockInput.answerCallerPrincipalId, "ivy-native-clock");
      assert.deepEqual(clockInput.reply, {
        result: { currentTimeAt: clockTime },
      });
      const clockSubstitution = (await tool("agent.answer", {
        operationId: "clock-substitution",
        identity: clockInput.identity,
        reply: { result: { currentTimeAt: 1 } },
      })) as Agent.Operation;
      assert.equal(clockSubstitution.phase, "failed");
      assert.equal(clockSubstitution.code, "native_clock_owned");
      assert.equal(
        second.sent.filter((value) => value["id"] === "local-clock").length,
        1,
      );
      second.emit({
        method: "serverRequest/resolved",
        params: { requestId: "local-clock", threadId: "fixture-thread" },
      });
      assert.equal(
        (
          (await tool("agent.inputs", {
            identity: clockInput.identity,
          })) as Agent.PendingInputPage
        ).items[0]!.state,
        "answered",
      );
    },
  );
