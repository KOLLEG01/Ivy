import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { IvyError } from "../packages/sdk/src/node.js";
import type { RpcClient } from "../packages/sdk/src/node.js";
import { PhoneJournal } from "../services/phone-bridge/src/runtime/journal.js";
import { PhoneVoiceNotices } from "../services/phone-bridge/src/runtime/voice-notices.js";
import type { PhoneVoiceWork } from "../services/phone-bridge/src/runtime/voice-work.js";
import { validatePhoneService } from "../services/phone-bridge/src/runtime/registry.js";

function fixture(t: test.TestContext) {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-notice-"));
  const journal = new PhoneJournal(root, {
    hostId: "fixture",
    serviceNodeId: "phone",
  });
  t.after(() => {
    journal.close();
    rmSync(root, { recursive: true, force: true });
  });
  const calls: { method: string; args: Record<string, unknown> }[] = [];
  let operation: unknown = null,
    loseReply = false,
    mainReady = true;
  const target = {
    serviceNodeId: "fixture.chat",
    expectedBridge: {
      principalId: "fixture.chat",
      callerPrincipalId: "phone",
      workspaceId: "main",
    },
    channel: {
      adapter: "whatsapp",
      accountId: "existing",
      conversationId: "main",
    },
  };
  const client = {
    async request(method: string, args: Record<string, unknown>) {
      calls.push({ method, args });
      if (method === "system.status") return { runtimeEpoch: randomUUID() };
      if (method === "objects.write") {
        validatePhoneService(
          "PhoneVoiceQuestion",
          (args["content"] as { value: unknown }).value,
        );
        return { object: { id: randomUUID() }, revision: { revision: 1 } };
      }
      if (method === "tools.list")
        return {
          provider: { node: { serviceNodeId: target.serviceNodeId } },
          items: [
            {
              qualifiedName: "chat." + args["namePrefix"],
              definitionHash: "hash",
              definition: {
                interfaceVersion: "1.2.0",
                annotations: { readOnlyHint: args["namePrefix"] !== "notify" },
              },
            },
          ],
          nextCursor: null,
        };
      if (method === "serviceNodes.get")
        return {
          serviceName: "chat-bridge",
          principalId: target.expectedBridge.principalId,
          connected: true,
          synced: true,
          ready: mainReady,
          serviceNodeId: target.serviceNodeId,
        };
      if (method === "tools.call") {
        if (args["qualifiedName"] === "chat.workspace")
          return {
            expectedBridge: target.expectedBridge,
            nativeOwnerReady: true,
            channels: [{ channel: target.channel }],
          };
        if (args["qualifiedName"] === "chat.operation") {
          if (operation) return operation;
          throw new IvyError("not_found", "No original notification.");
        }
        if (args["qualifiedName"] === "chat.notify") {
          operation = {
            request: {
              ...(args["arguments"] as object),
              expectedBridge: target.expectedBridge,
              channel: target.channel,
            },
            phase: "accepted",
          };
          if (loseReply) {
            loseReply = false;
            throw new IvyError(
              "transport_lost",
              "Synthetic lost notification reply.",
              "unknown",
            );
          }
          return operation;
        }
      }
      throw new Error("Unexpected request: " + method);
    },
  } as unknown as RpcClient;
  const work: PhoneVoiceWork = {
    threadId: randomUUID(),
    turnId: randomUUID(),
    fingerprint: "runtime",
    principalId: "caller",
    policyHash: "policy",
    partyKey: "party",
    callIds: [randomUUID()],
    attachedCallId: null,
    state: "waiting_input",
    endedAt: Date.now(),
    deadline: Date.now() + 300_000,
    revision: 1,
    interruptId: null,
    error: null,
  };
  const input = {
    id: 17,
    inputToken: "sha256:" + "a".repeat(64),
    method: "item/tool/requestUserInput",
    params: {
      threadId: work.threadId,
      turnId: work.turnId,
      questions: [{ id: "destination", question: "Which destination?" }],
    },
    replySchema: { type: "object" },
  };
  return {
    journal,
    calls,
    work,
    input,
    client,
    notice: () => new PhoneVoiceNotices(client, journal, "phone"),
    loseReply: () => {
      loseReply = true;
    },
    mainOffline: () => {
      mainReady = false;
    },
    retry: () => {
      const saved = journal.voiceNotice(input.inputToken) as {
        retryAt: number;
      };
      saved.retryAt = 0;
      journal.saveVoiceNotice(input.inputToken, saved);
    },
  };
}

test("native question uses only the official existing Main notification path and retained source", async (t) => {
  const f = fixture(t);
  assert.equal(await f.notice().notify(f.work, f.input, () => true), true);
  const notify = f.calls.filter(
    (call) =>
      call.method === "tools.call" &&
      call.args["qualifiedName"] === "chat.notify",
  );
  assert.equal(notify.length, 1);
  const request = notify[0]!.args["arguments"] as Record<string, unknown>;
  assert.deepEqual(Object.keys(request).sort(), [
    "action",
    "operationId",
    "source",
    "text",
  ]);
  assert.match(String(request["text"]), /five-minute deadline keeps running/);
  assert.match(String(request["text"]), /phone_bridge_status/);
  assert.ok(String(request["text"]).includes(f.work.threadId));
  assert.doesNotMatch(String(request["text"]), /phone_bridge_work|phone_bridge_voice_inputs/);
  assert.equal(f.calls.find(call => call.method === 'objects.write')!.args['contractVersion'], '1.1.0');
  assert.equal(await f.notice().notify(f.work, f.input, () => true), true);
  assert.equal(
    f.calls.filter((call) => call.method === "objects.write").length,
    1,
  );
  assert.equal(
    f.calls.filter((call) => call.args["qualifiedName"] === "chat.notify")
      .length,
    1,
  );
});

test("lost Main acknowledgement is reconciled by exact operation without another notification", async (t) => {
  const f = fixture(t);
  f.loseReply();
  assert.equal(await f.notice().notify(f.work, f.input, () => true), false);
  const saved = f.journal.voiceNotice(f.input.inputToken);
  f.retry();
  assert.equal(await f.notice().notify(f.work, f.input, () => true), true);
  assert.equal(
    f.calls.filter((call) => call.args["qualifiedName"] === "chat.notify")
      .length,
    1,
  );
  assert.equal(
    (f.journal.voiceNotice(f.input.inputToken) as { operationId: string })
      .operationId,
    (saved as { operationId: string }).operationId,
  );
});

test("expiry or unavailable Main never implies consent and delivery retries are bounded", async (t) => {
  const f = fixture(t);
  assert.equal(await f.notice().notify(f.work, f.input, () => false), true);
  assert.equal(f.calls.length, 0);
  f.mainOffline();
  for (let i = 0; i < 6; i++) {
    assert.equal(await f.notice().notify(f.work, f.input, () => true), i === 5);
    f.retry();
  }
  const count = f.calls.length;
  assert.equal(await f.notice().notify(f.work, f.input, () => true), true);
  assert.equal(f.calls.length, count);
  assert.equal(
    f.calls.some((call) => call.args["qualifiedName"] === "chat.notify"),
    false,
  );
});
