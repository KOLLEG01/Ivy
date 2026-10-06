import { unpersistedNativePlan } from "./fixtures/native-plan.js";
import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  canonical,
  digest,
  hashJson,
} from "../packages/contracts/src/canonical.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import type {
  Chat,
  OperationName,
  Params,
  Result,
  Wire,
} from "../packages/contracts/src/generated.js";
import type { RpcClient } from "../packages/sdk/src/client.js";
import { HiveKernel } from "../services/hive/src/kernel.js";
import type { ConnectionContext } from "../services/hive/src/kernel.js";
import { ChatAdmission } from "../services/chat-bridge/src/admission.js";
import { ChatStore } from "../services/chat-bridge/src/store.js";
import { ChatOperations } from "../services/chat-bridge/src/operations.js";
import { ChatOutbox } from "../services/chat-bridge/src/outbox.js";
import { chatContracts } from "../services/chat-bridge/src/schema.js";
import { chatNativeCatalog } from "../services/chat-bridge/src/native-evidence.js";

const channel: Chat.WhatsAppChannelConfiguration['channel'] = {
  adapter: "whatsapp",
  accountId: "synthetic",
  channelId: "browser",
};
const other: Chat.Channel = { ...channel, channelId: "private-browser" };
const hasCode = (code: string) => (error: unknown) =>
  error instanceof IvyError && error.code === code;
const fields = (params: Params<"objects.write">) =>
  params.content.encoding === "json"
    ? (params.content.value as Record<string, unknown>)
    : {};
async function fixture(t: { after(callback: () => void): void }) {
  const dataRoot = mkdtempSync(join(tmpdir(), "ivy-chat-outbox-")),
    stores: ChatStore[] = [];
  const definition: Chat.Definition = {
    workspaceId: "synthetic-chat",
    principalId: "chat-owner",
    rootObjectId: null,
    project: {
      serviceNodeId: "synthetic-agent",
      namespace: "codex",
      kind: "project",
      nativeId: "/fixture",
    },
    nativePlan: unpersistedNativePlan("0.154.0"),
    channels: [
      { channel, displayName: "Shared browser" },
    ],
  };
  const admission = new ChatAdmission(definition),
    credentialDigest = digest(randomUUID());
  const kernel = new HiveKernel({
    filename: ":memory:",
    publicBaseUrl: "https://chat.test/ivy",
    version: "test",
    buildId: digest("outbox"),
    credentials: [{ principalId: "chat-owner", digest: credentialDigest }],
  });
  t.after(() => {
    for (const store of stores) store.close();
    kernel.close();
    rmSync(dataRoot, { recursive: true, force: true });
  });
  let after:
      | ((
          params: Params<"objects.write">,
          result: Result<"objects.write">,
        ) => void | Promise<void>)
      | null = null,
    accesses = 0;
  const client = (node: string): RpcClient => {
    let context: ConnectionContext = {
      credentialDigest,
      principalId: "chat-owner",
      transport: "ws",
    };
    const execute = <M extends OperationName>(
      method: M,
      params: Params<M>,
    ): Result<M> => {
      const result = kernel.execute(context, {
        jsonrpc: "2.0",
        id: randomUUID(),
        method,
        params,
      });
      if (result.kind !== "result")
        throw Error("Outbox must perform no native dispatch.");
      return result.value as Result<M>;
    };
    const connected = execute("service.connect", {
      serviceNodeId: node,
      serviceName: "chat-bridge",
      hostId: "fixture",
      version: "test",
      buildId: digest(node),
      hiveProtocol: 1,
    });
    context = {
      ...context,
      serviceNodeId: node,
      generation: connected.generation,
    };
    const contracts = chatContracts();
    execute("registry.sync", {
      namespaces: [],
      contracts,
      requiredContracts: contracts.map((value) => ({
        key: value.key,
        readVersions: [value.version],
        writeVersions: [value.version],
      })),
    });
    execute("service.heartbeat", { ready: true, diagnostics: [] });
    return {
      async request<M extends OperationName>(method: M, params: Params<M>) {
        accesses++;
        const value = execute(method, params);
        if (method === "objects.write")
          await after?.(
            params as Params<"objects.write">,
            value as Result<"objects.write">,
          );
        return value;
      },
    };
  };
  const firstClient = client("chat-a"),
    secondClient = client("chat-b"),
    makeStore = (connection: RpcClient) => {
      const value = new ChatStore(connection, null, dataRoot);
      stores.push(value);
      return value;
    },
    store = makeStore(firstClient),
    at = new Date().toISOString();
  const binding = await store.write(
    "chat-bridge/binding",
    {
      schemaVersion: 1,
      workspaceId: definition.workspaceId,
      definitionHash: admission.definitionHash,
      project: definition.project,
      primary: {
        ...definition.project,
        kind: "thread",
        nativeId: "original-main",
      },
      nativePlan: definition.nativePlan,
      createdAt: at,
      operationId: "fixture-main",
      predecessor: null,
    },
    randomUUID(),
    { create: { parentId: null, name: "Fixture binding" } },
  );
  let sequence = 1;
  const add = async (
    text = "Saved reply",
    selected: Chat.Channel = channel,
    artifacts: Chat.Artifact[] = [],
    inputText = "Original " + text.slice(0, 100),
  ) => {
    const id = randomUUID(),
      input: Chat.Input = {
        schemaVersion: 1,
        workspaceId: definition.workspaceId,
        definitionHash: admission.definitionHash,
        identity: {
          channel: selected,
          senderPrincipalId: "bob",
          messageId: id + "-message",
        },
        operationId: id,
        requestHash: hashJson({ id }),
        binding,
        sequence: sequence++,
        payload: { text: inputText, images: [] },
        state: "queued",
        nativeCalls: { resume: null, turn: null, interrupt: null },
        turnId: null,
        epoch: null,
        result: null,
        error: null,
        cancellation: null,
        admittedAt: at,
        updatedAt: at,
        finishedAt: null,
      };
    const inputPin = await store.write(
      "chat-bridge/input",
      input,
      randomUUID(),
      { create: { parentId: null, name: "Input fixture " + id } },
    );
    // Terminal input/result are synthetic collection fixtures. These tests establish delivery,
    // not native result collection or authority, and never claim real model acceptance.
    const result = await store.write(
      "chat-bridge/result",
      {
        schemaVersion: 1,
        inputId: inputPin.objectId,
        binding,
        turnId: "turn-" + id,
        nativeState: "completed",
        evidence: [inputPin],
        textParts: [],
        createdAt: at,
      },
      randomUUID(),
      { create: { parentId: inputPin.objectId, name: "Fixture result" } },
    );
    const complete = await store.write(
      "chat-bridge/input",
      {
        ...input,
        state: "completed",
        result,
        turnId: "turn-" + id,
        epoch: "synthetic-epoch",
        finishedAt: at,
      },
      randomUUID(),
      { objectId: inputPin.objectId, expectedRevision: inputPin.revision },
    );
    const immutable = {
      schemaVersion: 1 as const,
      workspaceId: definition.workspaceId,
      definitionHash: admission.definitionHash,
      channel: selected,
      origin: {
        kind: "native" as const,
        inputId: inputPin.objectId,
        binding,
        result,
      },
      sequence: sequence++,
      partIndex: 0,
      text,
      artifacts,
      createdAt: at,
    };
    const reply: Chat.Reply = {
      ...immutable,
      immutableHash: hashJson(immutable),
      state: "pending",
      firstOfferedAt: null,
      confirmedAt: null,
    };
    const replyPin = await store.write(
      "chat-bridge/reply",
      reply,
      randomUUID(),
      { create: { parentId: null, name: "Reply fixture " + id } },
    );
    return { input: complete, reply: replyPin, result };
  };
  const first = new ChatOutbox(new ChatOperations(store, admission)),
    second = new ChatOutbox(
      new ChatOperations(makeStore(secondClient), admission),
    );
  const receive = (
    operationId: string,
    caller = "alice",
    selected: Chat.Channel = channel,
    afterSequence = 0,
    limit = 8,
  ): Chat.ReceiveRequest => ({
    action: "receive",
    operationId,
    expectedBridge: admission.expected(caller),
    channel: selected,
    afterSequence,
    limit,
  });
  const ack = (
    operationId: string,
    offerId: string,
    replyIds: string[],
    caller = "alice",
  ): Chat.AcknowledgeRequest => ({
    action: "acknowledge",
    operationId,
    expectedBridge: admission.expected(caller),
    offerId,
    replyIds,
  });
  return {
    first,
    second,
    store,
    add,
    binding,
    admission,
    receive,
    ack,
    accesses: () => accesses,
    intercept: (callback: typeof after) => {
      after = callback;
    },
  };
}

test("an interrupted acknowledgement retains its older offer until recovery completes", async (t) => {
  const f = await fixture(t), added = await f.add();
  const received = await f.first.receive("alice", f.receive("old-offer"));
  if (received.outcome?.action !== "receive") throw Error("Expected offer.");
  const operation = (await f.first.operations.find("alice", received.operationId))!;
  await f.store.write("chat-bridge/operation",
    { ...operation.value, updatedAt: "2000-01-01T00:00:00.000Z" }, "age-offer",
    { objectId: operation.pin.objectId, expectedRevision: operation.pin.revision });
  const request = f.ack("interrupted-ack", received.outcome.offer.object.objectId, [added.reply.objectId]);
  // A process can stop after accepting the acknowledgement and before confirming replies.
  await f.first.operations.begin("alice", request);
  const maintenance = f.store as unknown as { expireOperations(now: number, force: boolean): void };
  maintenance.expireOperations(Date.now(), true);
  assert.equal((await f.second.acknowledge("alice", request)).phase, "succeeded");
  assert.equal((await f.store.read("chat-bridge/reply", added.reply.objectId)).value.state, "confirmed");
  maintenance.expireOperations(Date.now(), true);
  assert.equal(await f.first.operations.find("alice", received.operationId), null);
});

test("a retained acknowledgement remains idempotent after its older offer expires", async (t) => {
  const f = await fixture(t),
    added = await f.add();
  const received = await f.first.receive("alice", f.receive("old-offer"));
  if (received.outcome?.action !== "receive") throw Error("Expected offer.");
  const operation = (await f.first.operations.find(
    "alice",
    received.operationId,
  ))!;
  await f.store.write(
    "chat-bridge/operation",
    { ...operation.value, updatedAt: "2000-01-01T00:00:00.000Z" },
    "age-offer",
    {
      objectId: operation.pin.objectId,
      expectedRevision: operation.pin.revision,
    },
  );
  const request = f.ack("recent-ack", received.outcome.offer.object.objectId, [
    added.reply.objectId,
  ]);
  const acknowledged = await f.first.acknowledge("alice", request);
  (
    f.store as unknown as {
      expireOperations(now: number, force: boolean): void;
    }
  ).expireOperations(Date.now(), true);
  assert.equal(
    await f.first.operations.find("alice", received.operationId),
    null,
  );
  assert.deepEqual(await f.second.acknowledge("alice", request), acknowledged);
  await assert.rejects(
    f.second.acknowledge("alice", { ...request, replyIds: [randomUUID()] }),
    hasCode("mutation_conflict"),
  );
});

test("Chat history stays read-only; receive retains original channel replies and acknowledgement alone confirms its selected subset", async (t) => {
  const f = await fixture(t),
    a = await f.add("First"),
    privateReply = await f.add("Private", other),
    b = await f.add("Delayed second");
  const history = await f.first.history("alice", {
    expectedBridge: f.admission.expected("alice"),
    channel,
    afterSequence: 0,
    limit: 3,
  });
  assert.deepEqual(
    history.entries.map((entry) => entry.data.sequence),
    [1, 2, 5],
  );
  assert.equal(history.throughSequence, 5);
  assert.equal(history.hasMore, true);
  assert.equal(
    (await f.store.read("chat-bridge/reply", a.reply.objectId)).value.state,
    "pending",
  );
  const received = await f.first.receive("alice", f.receive("receive"));
  if (received.outcome?.action !== "receive") throw Error("Expected offer.");
  assert.deepEqual(
    received.outcome.replies.map((entry) => entry.object),
    [a.reply, b.reply],
  );
  assert.equal(received.outcome.offer.data.throughSequence, 6);
  assert.deepEqual(
    received.outcome.replies.map(
      (entry) =>
        entry.data.origin.kind === "native" && entry.data.origin.binding,
    ),
    [f.binding, f.binding],
  );
  for (const reply of [a.reply, b.reply])
    assert.equal(
      (await f.store.read("chat-bridge/reply", reply.objectId)).value.state,
      "outcome_unknown",
    );
  assert.equal(
    (await f.store.read("chat-bridge/reply", privateReply.reply.objectId)).value
      .state,
    "pending",
  );
  const request = f.ack("ack", received.outcome.offer.object.objectId, [
    a.reply.objectId,
  ]);
  const confirmed = await f.second.acknowledge("alice", request);
  assert.equal(confirmed.phase, "succeeded");
  assert.equal(
    (await f.store.read("chat-bridge/reply", a.reply.objectId)).value.state,
    "confirmed",
  );
  assert.equal(
    (await f.store.read("chat-bridge/reply", b.reply.objectId)).value.state,
    "outcome_unknown",
  );
  assert.deepEqual(await f.first.acknowledge("alice", request), confirmed);
  assert.deepEqual(
    await f.second.receive("alice", f.receive("receive")),
    received,
    "Original response retains its original offered revisions after acknowledgement.",
  );
  const reoffered = await f.second.receive("alice", f.receive("reoffer"));
  assert.equal(reoffered.phase, "succeeded");
  assert.equal(
    (await f.store.read("chat-bridge/reply", a.reply.objectId)).value.state,
    "confirmed",
  );
});

test("Chat outbox refuses unknown channels and mismatched caller bindings before accepting acknowledgement", async (t) => {
  const f = await fixture(t),
    a = await f.add(),
    b = await f.add("Other part");
  const before = f.accesses();
  await assert.rejects(
    f.first.receive(
      "alice",
      f.receive("private", "alice", { ...other, channelId: "missing" }),
    ),
    hasCode("chat_sender_refused"),
  );
  await assert.rejects(
    f.first.history("mallory", {
      expectedBridge: f.admission.expected("alice"),
      channel,
      afterSequence: 0,
      limit: 8,
    }),
    hasCode("chat_definition_mismatch"),
  );
  assert.equal(f.accesses(), before);
  const offered = await f.first.receive(
    "alice",
    f.receive("limited", "alice", channel, 0, 1),
  );
  if (offered.outcome?.action !== "receive") throw Error("Expected offer.");
  await assert.rejects(
    f.second.acknowledge(
      "bob",
      f.ack(
        "wrong-caller",
        offered.outcome.offer.object.objectId,
        [a.reply.objectId],
        "bob",
      ),
    ),
    hasCode("chat_offer_mismatch"),
  );
  await assert.rejects(
    f.first.acknowledge(
      "alice",
      f.ack("not-offered", offered.outcome.offer.object.objectId, [
        b.reply.objectId,
      ]),
    ),
    hasCode("chat_offer_mismatch"),
  );
  assert.equal(await f.first.operations.find("alice", "not-offered"), null);
  assert.equal(await f.second.operations.find("bob", "wrong-caller"), null);
  await assert.rejects(
    f.first.receive("alice", f.receive("limited", "alice", channel, 0, 2)),
    hasCode("mutation_conflict"),
  );
});

for (const boundary of ["offered reply", "confirmed reply"])
  test(
    "Chat outbox recovers original " +
      boundary +
      " after committed response loss",
    async (t) => {
      const f = await fixture(t),
        a = await f.add("First"),
        b = await f.add("Second");
      let lost = false;
      f.intercept((params, result) => {
        const value = fields(params),
          matches =
            boundary === "offer"
              ? result.object.contractKey === "chat-bridge/offer"
              : boundary === "offered reply"
                ? result.object.contractKey === "chat-bridge/reply" &&
                  value["state"] === "outcome_unknown"
                : boundary === "receive receipt"
                  ? result.object.contractKey === "chat-bridge/operation" &&
                    value["phase"] === "succeeded" &&
                    (value["request"] as { action: string }).action ===
                      "receive"
                  : boundary === "acknowledgement"
                    ? result.object.contractKey ===
                      "chat-bridge/acknowledgement"
                    : boundary === "confirmed reply"
                      ? result.object.contractKey === "chat-bridge/reply" &&
                        value["state"] === "confirmed"
                      : result.object.contractKey === "chat-bridge/operation" &&
                        value["phase"] === "succeeded" &&
                        (value["request"] as { action: string }).action ===
                          "acknowledge";
        if (!lost && matches) {
          lost = true;
          throw new IvyError(
            "outcome_unknown",
            "Lost committed outbox response.",
            "unknown",
          );
        }
      });
      const receiving = f.receive("receive");
      let received: Chat.Operation;
      if (["offer", "offered reply", "receive receipt"].includes(boundary)) {
        await assert.rejects(
          f.first.receive("alice", receiving),
          hasCode("outcome_unknown"),
        );
        assert.equal(lost, true);
        f.intercept(null);
        received = await f.second.receive("alice", receiving);
      } else received = await f.first.receive("alice", receiving);
      if (received.outcome?.action !== "receive")
        throw Error("Expected offer.");
      const request = f.ack("ack", received.outcome.offer.object.objectId, [
        a.reply.objectId,
        b.reply.objectId,
      ]);
      if (!lost) {
        await assert.rejects(
          f.first.acknowledge("alice", request),
          hasCode("outcome_unknown"),
        );
        assert.equal(lost, true);
        f.intercept(null);
      }
      const acknowledgement = await f.second.acknowledge("alice", request);
      assert.equal(acknowledgement.phase, "succeeded");
      for (const reply of [a.reply, b.reply])
        assert.equal(
          (await f.store.read("chat-bridge/reply", reply.objectId)).value.state,
          "confirmed",
        );
      assert.deepEqual(await f.first.receive("alice", receiving), received);
      assert.deepEqual(
        await f.first.acknowledge("alice", request),
        acknowledgement,
      );
    },
  );

test("concurrent Chat offers and acknowledgement aliases never downgrade confirmation or duplicate the original offer", async (t) => {
  const f = await fixture(t),
    a = await f.add();
  const same = await Promise.all([
    f.first.receive("alice", f.receive("same")),
    f.second.receive("alice", f.receive("same")),
  ]);
  assert.deepEqual(same[0], same[1]);
  const original = same[0]!;
  if (original.outcome?.action !== "receive") throw Error("Expected offer.");
  const offerId = original.outcome.offer.object.objectId;
  const results = await Promise.all([
    f.first.acknowledge("alice", f.ack("one", offerId, [a.reply.objectId])),
    f.second.acknowledge("alice", f.ack("two", offerId, [a.reply.objectId])),
    f.first.receive("bob", f.receive("other-tab", "bob")),
  ]);
  assert.ok(results.every((result) => result.phase === "succeeded"));
  const confirmed = await f.store.read("chat-bridge/reply", a.reply.objectId);
  assert.equal(confirmed.value.state, "confirmed");
  await f.first.receive("alice", f.receive("once-more"));
  assert.deepEqual(
    (await f.store.read("chat-bridge/reply", a.reply.objectId)).pin,
    confirmed.pin,
  );
});

test("Chat history advances over pages of hidden service inputs without skipping visible entries", async (t) => {
  const f = await fixture(t);
  const first = await f.add("Visible answer");
  const source = await f.store.read("chat-bridge/input", first.input.objectId);
  const savedReply = await f.store.read(
    "chat-bridge/reply",
    first.reply.objectId,
  );
  const {
    state: _state,
    firstOfferedAt: _offered,
    confirmedAt: _confirmed,
    immutableHash: _hash,
    ...immutable
  } = savedReply.value;
  const updated = { ...immutable, sequence: 10 };
  await f.store.write(
    "chat-bridge/reply",
    { ...savedReply.value, ...updated, immutableHash: hashJson(updated) },
    randomUUID(),
    {
      objectId: savedReply.pin.objectId,
      expectedRevision: savedReply.pin.revision,
    },
  );
  for (let index = 0; index < 6; index++) {
    const id = "service-" + index;
    await f.store.write(
      "chat-bridge/input",
      {
        ...source.value,
        sequence: 3 + index,
        operationId: id,
        identity: { ...source.value.identity, messageId: id },
      },
      randomUUID(),
      { create: { parentId: null, name: id } },
    );
  }
  const last = await f.store.write(
    "chat-bridge/input",
    { ...source.value, sequence: 9 },
    randomUUID(),
    { create: { parentId: null, name: "last-visible" } },
  );
  let afterSequence = 0;
  const ids: string[] = [];
  for (let page = 0; page < 8; page++) {
    const history = await f.first.history("alice", {
      expectedBridge: f.admission.expected("alice"),
      channel,
      afterSequence,
      limit: 1,
    });
    ids.push(...history.entries.map((entry) => entry.object.objectId));
    if (!history.hasMore) break;
    assert.ok(history.throughSequence > afterSequence);
    afterSequence = history.throughSequence;
  }
  assert.deepEqual(ids, [
    first.input.objectId,
    last.objectId,
    first.reply.objectId,
  ]);
});

test("empty Chat offers preserve their cursor, while an altered saved reply cannot be delivered as its original", async (t) => {
  const f = await fixture(t),
    empty = await f.first.receive(
      "alice",
      f.receive("empty", "alice", channel, 10),
    );
  if (empty.outcome?.action !== "receive") throw Error("Expected empty offer.");
  assert.deepEqual(empty.outcome.replies, []);
  assert.equal(empty.outcome.offer.data.throughSequence, 10);
  const created = await f.add(),
    original = await f.store.read("chat-bridge/reply", created.reply);
  await f.store.write(
    "chat-bridge/reply",
    { ...original.value, text: "Rewritten reply" },
    randomUUID(),
    {
      objectId: created.reply.objectId,
      expectedRevision: created.reply.revision,
    },
  );
  await assert.rejects(
    f.second.receive("alice", f.receive("altered")),
    hasCode("chat_identity_conflict"),
  );
});

test("Chat history and receive stop within the complete byte limit without advancing past omitted replies", async (t) => {
  const f = await fixture(t),
    source = await f.add("Artifact source", other),
    label = "\0".repeat(256),
    artifact: Chat.Artifact = {
      object: source.result,
      contentHash: digest("fixture-artifact"),
      byteLength: 1,
      mediaType: "text/plain",
      label,
    };
  const added = [];
  for (let i = 0; i < 8; i++)
    added.push(
      await f.add(
        "\0".repeat(16384),
        channel,
        Array.from({ length: 32 }, () => artifact),
        "\0".repeat(65536),
      ),
    );
  const received = await f.first.receive("alice", f.receive("bounded"));
  if (received.outcome?.action !== "receive")
    throw Error("Expected bounded offer.");
  assert.ok(Buffer.byteLength(canonical(received)) <= 1024 * 1024);
  assert.ok(
    received.outcome.replies.length > 0 && received.outcome.replies.length < 8,
  );
  const through = received.outcome.offer.data.throughSequence;
  assert.equal(through, received.outcome.replies.at(-1)!.data.sequence);
  const next = await f.second.receive(
    "alice",
    f.receive("next", "alice", channel, through),
  );
  if (next.outcome?.action !== "receive") throw Error("Expected next offer.");
  assert.equal(
    next.outcome.replies[0]!.object.objectId,
    added[received.outcome.replies.length]!.reply.objectId,
  );
  const history = await f.first.history("alice", {
    expectedBridge: f.admission.expected("alice"),
    channel,
    afterSequence: 0,
    limit: 8,
  });
  assert.ok(Buffer.byteLength(canonical(history)) <= 1024 * 1024);
  assert.ok(history.hasMore);
  assert.equal(history.throughSequence, history.entries.at(-1)!.data.sequence);
});
