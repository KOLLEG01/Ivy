import { checkedNativeContract } from "../packages/contracts/src/checked-native-contract.js";
import { validateNativePlanDraft } from "../packages/sdk/src/native-plan.js";
import {
  nativePlanDraft,
  unpersistedNativePlan,
} from "./fixtures/native-plan.js";
import { validateChatNativeDraft } from "../services/chat-bridge/src/native-plan.js";
import { chatContracts } from "../services/chat-bridge/src/schema.js";
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bundledSchema } from "../packages/contracts/src/bundle.js";
import { SchemaValidators } from "../packages/contracts/src/schema.js";
import {
  validateChat,
  validateShared,
} from "../packages/contracts/src/validation.js";
import { digest } from "../packages/contracts/src/canonical.js";
import type { Agent } from "../packages/contracts/src/generated.js";
import catalog from "../specs/schemas/chat.operations.json" with { type: "json" };

const examples = JSON.parse(
  readFileSync("specs/examples/chat-transitions.json", "utf8"),
) as {
  cases: Array<{
    name: string;
    definition: string;
    valid: boolean;
    value: Record<string, unknown>;
  }>;
};
const pin = { objectId: "fixture", revision: 1 },
  time = "2026-09-07T03:00:00.000Z";

test("ChatBridge contracts fit unchanged Hive validation limits and retain strict action and media schemas", () => {
  const validators = new SchemaValidators();
  for (const contract of catalog.contracts)
    validators.compile(bundledSchema(contract.definition, catalog.schema));
  for (const contract of chatContracts().filter(
    (value) => value.mediaType !== "application/json",
  ))
    validateShared("DataContract", contract);
  assert.ok(
    chatContracts()
      .filter((value) => value.mediaType !== "application/json")
      .every((value) => value.retention.objects.mode === "owned"),
  );
  assert.deepEqual(
    chatContracts().find((value) => value.key === "chat-bridge/conversation")!
      .retention.objects,
    { mode: "retain" },
  );
  for (const key of [
    "chat-bridge/input",
    "chat-bridge/result",
    "chat-bridge/reply",
  ])
    assert.deepEqual(
      chatContracts().find((value) => value.key === key)!.retention.objects,
      { mode: "expire", maximumAgeDays: 90 },
    );
  for (const definition of Object.values(catalog.operations)) {
    const input = bundledSchema(definition.input, catalog.schema);
    assert.equal(input["type"], "object");
    validators.compile(input);
    validators.compile(bundledSchema(definition.output, catalog.schema));
  }
  assert.equal(catalog.contracts.length, 12);
  assert.equal(catalog.mediaContracts.length, 5);
  assert.deepEqual(Object.keys(catalog.operations).sort(), [
    "acknowledge",
    "cancel",
    "createMain",
    "history",
    "input",
    "notice",
    "notify",
    "operation",
    "receive",
    "retryResult",
    "send",
    "workspace",
  ]);
  for (const item of examples.cases) {
    if (item.valid)
      assert.doesNotThrow(
        () => validateChat(item.definition, item.value),
        item.name,
      );
    else
      assert.throws(() => validateChat(item.definition, item.value), item.name);
  }
});

test("ChatBridge language is optional, bounded to supported locales and outside the Definition", () => {
  const settings = {
    definition: {
      workspaceId: "workspace",
      principalId: "principal",
      rootObjectId: null,
      project: {
        serviceNodeId: "agent",
        namespace: "codex",
        kind: "project",
        nativeId: "project",
      },
      nativePlan: unpersistedNativePlan(),
      channels: [
        {
          channel: {
            adapter: "whatsapp",
            accountId: "account",
            channelId: "channel",
          },
          displayName: "WhatsApp",
        },
      ],
    },
    pollMs: 1000,
  };
  assert.doesNotThrow(() => validateChat("Settings", settings));
  assert.doesNotThrow(() =>
    validateChat("Settings", { ...settings, language: "en" }),
  );
  assert.doesNotThrow(() =>
    validateChat("Settings", { ...settings, language: "de" }),
  );
  assert.throws(() =>
    validateChat("Settings", { ...settings, language: "fr" }),
  );
  assert.throws(() =>
    validateChat("Settings", {
      ...settings,
      definition: { ...settings.definition, language: "de" },
    }),
  );
});

test("ChatBridge state shapes reject invented completion and unacknowledged confirmation", () => {
  const input = examples.cases.find(
    (item) => item.definition === "Input",
  )!.value;
  const reply = examples.cases.find(
    (item) => item.definition === "Reply",
  )!.value;
  for (const invalid of [
    { ...input, state: "completed" },
    { ...input, state: "running" },
    { ...input, state: "cancelled", finishedAt: time },
    { ...input, finishedAt: time },
    { ...input, turnId: "unstarted" },
    { ...input, state: "failed", finishedAt: time },
    {
      ...input,
      state: "outcome_unknown",
      error: { code: "lost", outcome: "not_executed", detail: "Lost response" },
    },
  ])
    assert.throws(() => validateChat("Input", invalid));
  validateChat("Input", {
    ...input,
    state: "outcome_unknown",
    error: {
      code: "lost",
      outcome: "unknown",
      detail: "Original owner outcome must be reconciled.",
    },
  });
  validateChat("Input", {
    ...input,
    state: "completed",
    turnId: "turn",
    epoch: "epoch",
    result: pin,
    finishedAt: time,
  });
  for (const invalid of [
    { ...reply, state: "confirmed" },
    { ...reply, state: "outcome_unknown" },
    { ...reply, confirmedAt: time },
  ])
    assert.throws(() => validateChat("Reply", invalid));
  validateChat("Reply", {
    ...reply,
    state: "outcome_unknown",
    firstOfferedAt: time,
  });
  validateChat("Reply", {
    ...reply,
    state: "confirmed",
    firstOfferedAt: time,
    confirmedAt: time,
  });
  const image = {
    object: pin,
    contentHash: digest("image"),
    byteLength: 1024,
    mediaType: "image/png",
    label: "Example",
  };
  validateChat("Image", image);
  for (const invalid of [
    { ...image, byteLength: 2097153 },
    { ...image, mediaType: "image/svg+xml" },
    { ...image, url: "https://elsewhere.invalid/image" },
  ])
    assert.throws(() => validateChat("Image", invalid));
  assert.throws(() =>
    validateChat("Payload", { text: "x".repeat(65537), images: [] }),
  );
  assert.throws(() =>
    validateChat("Payload", { text: "", images: [image, image, image] }),
  );
  const ack = examples.cases.find(
    (item) => item.definition === "AcknowledgeRequest",
  )!.value;
  assert.throws(() =>
    validateChat("AcknowledgeRequest", { ...ack, replyIds: [] }),
  );
  assert.throws(() =>
    validateChat("AcknowledgeRequest", { ...ack, replyIds: ["same", "same"] }),
  );
});

test("ChatBridge templates retain each exact native schema while reserving conversation and browser input fields", () => {
  for (const version of ["0.154.0"]) {
    const native = JSON.parse(
      readFileSync(`specs/native/codex-${version}/catalog.json`, "utf8"),
    ) as Agent.Catalog;
    const plan = nativePlanDraft(version, {}),
      contract = checkedNativeContract(version).contract;
    const validatePlan = (value: unknown) =>
      validateNativePlanDraft(contract, value, "chat");
    validateChat("NativePlan", unpersistedNativePlan(version));
    validatePlan(plan);
    for (const invalid of [
      { ...plan, turnStart: { input: [] } },
      { ...plan, turnStart: { threadId: "other" } },
      { ...plan, turnStart: { clientUserMessageId: "invented" } },
      { ...plan, threadResume: { path: "/another/thread" } },
      { ...plan, catalogSourceHash: digest("wrong") },
      { ...plan, nativeVersion: "unverified" },
    ])
      assert.throws(() => validatePlan(invalid));
    const params = {
      threadId: "saved-main",
      input: [
        { type: "text", text: "Synthetic" },
        { type: "image", url: "data:image/png;base64,AA==" },
      ],
    };
    const validators = new SchemaValidators();
    validators.validate(
      native.clientRequests.find((item) => item.method === "turn/start")!
        .inputSchema,
      params,
    );
    validateChatNativeDraft({
      nativeVersion: version,
      catalogSourceHash: native.sourceHash,
      method: "turn/start",
      params,
    });
    const versionSpecific = {
      nativeVersion: version,
      catalogSourceHash: native.sourceHash,
      method: "turn/start",
      params: { ...params, serviceTierForTurn: null },
    };
    validateChatNativeDraft(versionSpecific);
    // This establishes native argument shape only; these bytes deliberately are not a real image.
    assert.throws(() =>
      validateChatNativeDraft({
        nativeVersion: version,
        catalogSourceHash: native.sourceHash,
        method: "turn/start",
        params: {
          threadId: "saved-main",
          input: [{ type: "image", imageUrl: "guessed-field" }],
        },
      }),
    );
  }
});

test("Chat collection checkpoints distinguish retained progress, complete evidence and immutable result publication", () => {
  const collecting = {
    schemaVersion: 1,
    input: pin,
    binding: pin,
    turnCall: pin,
    turnId: "turn",
    epoch: "epoch",
    attempt: 1,
    previousAttempt: null,
    reads: [],
    nativeBytes: 0,
    state: "collecting",
    result: null,
    error: null,
    createdAt: time,
    updatedAt: time,
  };
  validateChat("Collection", collecting);
  const ready = {
    ...collecting,
    state: "ready",
    reads: [pin, pin, pin],
    nativeBytes: 2000,
  };
  validateChat("Collection", ready);
  validateChat("Collection", { ...ready, state: "complete", result: pin });
  validateChat("Collection", {
    ...collecting,
    attempt: 2,
    previousAttempt: pin,
    epoch: "new-epoch",
  });
  validateChat("Collection", {
    ...collecting,
    state: "failed",
    error: {
      code: "chat_collection_limit",
      outcome: "unknown",
      detail: "The original collection exceeded its bound.",
    },
  });
  for (const invalid of [
    { ...collecting, state: "ready" },
    { ...ready, state: "complete" },
    { ...collecting, state: "failed" },
    { ...collecting, result: pin },
    { ...collecting, attempt: 2 },
    { ...collecting, previousAttempt: pin },
    { ...collecting, reads: Array.from({ length: 1025 }, () => pin) },
    { ...collecting, nativeBytes: 67108865 },
  ])
    assert.throws(() => validateChat("Collection", invalid));
  // Semantic evidence verification additionally refuses duplicate pins/observations and unrelated bytes.
});

test("Chat result publication reserves an explicit bounded sequence range independently of user actions", () => {
  const value = {
    inputId: "input",
    collection: pin,
    result: pin,
    firstSequence: 10,
    partCount: 3,
    publishedParts: 0,
    createdAt: time,
  };
  validateChat("ResultPublication", value);
  validateChat("ResultPublication", { ...value, publishedParts: 3 });
  for (const invalid of [
    { ...value, partCount: 0 },
    { ...value, partCount: 65537 },
    { ...value, publishedParts: -1 },
    { ...value, firstSequence: 0 },
    { ...value, result: null },
    { ...value, operationId: "fake-user-action" },
  ])
    assert.throws(() => validateChat("ResultPublication", invalid));
  // Main validates head membership, publication progress and reserved sequence relationships.
});
