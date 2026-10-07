import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import catalog from "../specs/native/codex-0.159.2/catalog.json" with { type: "json" };
import { PhoneCodexVoice } from "../services/phone-bridge/src/runtime/codex-voice.js";
import type { PhoneCodexVoiceSettings } from "../services/phone-bridge/src/runtime/codex-voice.js";
import { PhoneJournal } from "../services/phone-bridge/src/runtime/journal.js";
import { PhoneAdmission } from "../services/phone-bridge/src/runtime/admission.js";
import type { startCodexProcess } from "../packages/host-runtime/src/codex-process.js";
import type { NativeNotification } from "../packages/host-runtime/src/codex-rpc.js";

const selection = {
  model: "gpt-6-sol" as const,
  reasoningEffort: "high" as const,
};
function fixture(t: test.TestContext) {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-codex-"));
  const journal = new PhoneJournal(
    root,
    { hostId: "fixture", serviceNodeId: "phone" },
    undefined,
    4,
  );
  const epoch = randomUUID();
  journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({
    incoming: ["recipient", "other"].map(id => ({ peerAddress: "127.0.0.1", transport: "udp" as const, fromUri: `sip:${id}@127.0.0.1` })),
    recipients: [{ id: "recipient", destination: "sip:recipient@127.0.0.1" }],
  });
  const settings: PhoneCodexVoiceSettings = {
    nativeExecutable: join(root, "codex.exe"),
    nativeExecutableHash: "sha256:" + "0".repeat(64),
    nativeVersion: "0.159.2",
    codexHome: join(root, "home"),
    cwd: join(root, "work"),
    projectId: randomUUID(),
    keepTaskLoaded: true,
    appServer: { mode: "owned-stdio" as const },
  };
  const requests: { method: string; params: Record<string, unknown> }[] = [];
  let notification!: (value: NativeNotification) => void,
    online = true,
    opens = 0;
  const threads = new Map<string, Record<string, unknown>>();
  const unloaded = new Set<string>();
  const deadLoops = new Set<string>();
  let unloadPause: Promise<void> | undefined;
  let preparationPause: Promise<void> | undefined;
  let losePreparationReply = false;
  const connect: typeof startCodexProcess = async (options) => {
    notification = options.onNotification;
    online = true;
    opens++;
    const rpc = {
      catalog,
      get connected() {
        return online;
      },
      async request(
        method: string,
        params: Record<string, unknown>,
        hooks: {
          requestId?: string;
          beforeResolve?: (id: string, value: unknown) => void;
        } = {},
      ) {
        requests.push({ method, params });
        let result: unknown = {};
        if (method === "project/read")
          result = { project: { id: settings.projectId } };
        if (method === "thread/start") {
          const thread = {
            id: randomUUID(),
            cwd: settings.cwd,
            projectId: settings.projectId,
            ephemeral: false,
            status: { type: "idle" },
            turns: [],
          };
          threads.set(thread.id, thread);
          result = { thread };
        }
        if (method === "thread/read" || method === "thread/resume") {
          const id = String(params["threadId"]);
          if (
            method === "thread/read" &&
            (!threads.has(id) || unloaded.has(id))
          )
            return {
              error: { code: -32600, message: `thread not loaded: ${id}` },
            };
          if (!threads.has(id))
            return {
              error: {
                code: -32600,
                message: `no rollout found for thread id ${id}`,
              },
            };
          if (method === "thread/resume") unloaded.delete(id);
          result = { thread: threads.get(id) };
        }
        if (method === "thread/unsubscribe") {
          await unloadPause;
          unloaded.add(String(params["threadId"]));
        }
        if (method === "turn/start") {
          const id = String(params["threadId"]);
          const turn = { id: randomUUID(), status: "completed", items: [
            { type: "userMessage", id: randomUUID(), clientId: String(params["clientUserMessageId"]) },
          ] };
          threads.get(id)!["turns"] = [turn];
          await preparationPause;
          // Completion can reach the client before the turn/start acknowledgement.
          notification({ method: "turn/completed", params: { threadId: id, turn } });
          if (losePreparationReply) { losePreparationReply = false; throw new Error("Preparation acknowledgement lost"); }
          result = { turn };
        }
        if (method === "thread/realtime/stop" && deadLoops.has(String(params["threadId"])))
          return { error: { code: -32603,
            message: "failed to stop realtime conversation: internal error; agent loop died unexpectedly" } };
        if (method === "thread/realtime/start")
          notification({
            method: "thread/realtime/sdp",
            params: {
              threadId: String(params["threadId"]),
              sdp: "remote answer",
            },
          });
        const reply = { result };
        hooks.beforeResolve?.(hooks.requestId ?? randomUUID(), reply);
        return reply;
      },
      answer() {},
    };
    return {
      rpc,
      home: settings.codexHome,
      epoch: randomUUID(),
      mode: "owned-stdio",
      completion: Promise.resolve({}),
      async close() {
        online = false;
      },
    } as unknown as Awaited<ReturnType<typeof startCodexProcess>>;
  };
  const voices: PhoneCodexVoice[] = [];
  const voice = () => {
    const value = new PhoneCodexVoice(
      settings,
      journal,
      { artifactRoot: root, dataRoot: root },
      undefined,
      connect,
    );
    voices.push(value);
    return value;
  };
  const admit = (principalId = "principal") =>
    journal.admitCall(
      policy.outgoing(epoch, randomUUID(), principalId, "recipient", "voice"),
    );
  t.after(async () => {
    for (const value of voices) await value.close();
    journal.close();
    rmSync(root, { recursive: true, force: true });
  });
  return {
    voice,
    journal,
    requests,
    admit,
    incoming: (caller = "recipient") => journal.admitCall(policy.incoming(epoch, randomUUID(), "incoming-user", {
      id: randomUUID(), direction: "incoming", state: "ringing", error: null, sipCallId: "fixture-wire",
      incoming: { sipCallId: "fixture-wire", peerAddress: "127.0.0.1", peerPort: 5060, transport: "udp", fromUri: `sip:${caller}@127.0.0.1` },
    })),
    settings,
    opens: () => opens,
    forgetTasks: () => threads.clear(),
    killLoops: () => { for (const id of threads.keys()) deadLoops.add(id); },
    reviveLoops: () => deadLoops.clear(),
    pauseUnload: (pause: Promise<void>) => {
      unloadPause = pause;
    },
    pausePreparation: (pause: Promise<void>) => { preparationPause = pause; },
    losePreparationReply: () => { losePreparationReply = true; },
    disconnect: () => {
      online = false;
    },
    notification: (value: NativeNotification) => notification(value),
  };
}

test("Incoming continuation keeps its exact task and speech context, still greets, and *0 restarts fresh", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.admit("mcp-agent"), threadId = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, threadId, "Discuss the lake walk.", "offer", () => {});
  const segment = { type: "transcriptSegment", id: "speech-1", realtimeSessionId: "fixture", role: "user", text: "My codeword is Seerose." };
  f.notification({ method: "thread/realtime/item/completed", params: { threadId, item: segment } });
  f.notification({ method: "thread/realtime/item/completed", params: { threadId, item: segment } });
  await voice.stop(first.callId);
  const before = f.requests.length, incoming = f.incoming();
  assert.equal(await voice.prepare(incoming, 0, selection), threadId);
  assert.deepEqual(f.requests.slice(before), []);
  await voice.start(incoming, 0, threadId, 'Begruesse den User mit "Servus"', "offer", () => {});
  await voice.connected(incoming.callId, async () => {});
  await voice.connected(incoming.callId, async () => {});
  const initial = f.requests.filter(x => x.method === "thread/realtime/start").at(-1)!.params["initialItems"];
  assert.deepEqual(initial, [{ role: "user", text: "Discuss the lake walk." }, { role: "user", text: "My codeword is Seerose." }]);
  assert.deepEqual(f.requests.filter(x => x.method === "thread/realtime/appendText").map(x => x.params),
    [{ threadId, role: "user", text: 'Begruesse den User mit "Servus"' }]);
  assert.equal(f.requests.filter(x => x.method === "turn/start").length, 1);
  const fresh = await voice.prepare(incoming, 1, selection);
  assert.notEqual(fresh, threadId);
  await voice.stop(incoming.callId, fresh);
  await voice.start(incoming, 1, fresh, 'Begruesse den User mit "Servus"', "offer", () => {});
  assert.deepEqual(f.requests.filter(x => x.method === "thread/realtime/start").at(-1)!.params["initialItems"],
    [{ role: "user", text: 'Begruesse den User mit "Servus"' }]);
});

test("An outgoing Ivy call never resumes the previous incoming conversation", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), old = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, old, "Previous context", "offer", () => {});
  await voice.stop(first.callId);
  const outgoing = f.admit(), fresh = await voice.prepare(outgoing, 0, selection);
  assert.notEqual(fresh, old);
  await voice.start(outgoing, 0, fresh, "New assignment", "offer", () => {});
  assert.deepEqual(f.requests.filter(x => x.method === "thread/realtime/start").at(-1)!.params["initialItems"],
    [{ role: "user", text: "New assignment" }]);
});

test("A different admitted incoming caller cannot inherit the previous conversation", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), old = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, old, "Private first context", "offer", () => {});
  await voice.stop(first.callId);
  assert.notEqual(await voice.prepare(f.incoming("other"), 0, selection), old);
});

test("An uncertain stop cannot expose the still-owned task as an incoming continuation", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), old = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, old, "Original context", "offer", () => {});
  f.killLoops();
  await assert.rejects(voice.stop(first.callId));
  const next = f.incoming();
  assert.notEqual(await voice.prepare(next, 0, selection), old);
  f.reviveLoops();
  await voice.stop(first.callId);
});

test("A continued native task is reconciled after reconnect without another READY preparation turn", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const firstVoice = f.voice(), first = f.admit(), old = await firstVoice.prepare(first, 0, selection);
  await firstVoice.start(first, 0, old, "Retained context", "offer", () => {});
  await firstVoice.stop(first.callId);
  await firstVoice.close();
  const voice = f.voice(), incoming = f.incoming();
  assert.equal(await voice.prepare(incoming, 0, selection), old);
  assert.equal(f.requests.filter(x => x.method === "turn/start").length, 1);
  assert.ok(f.requests.some(x => x.method === "thread/resume" && x.params["threadId"] === old));
});

test("A verified warm task starts without repeating native reads or settings, and errors invalidate that shortcut", async (t) => {
  const f = fixture(t), voice = f.voice();
  await voice.prewarm("principal", selection);
  const before = f.requests.length;
  const call = f.admit(), threadId = await voice.prepare(call, 0, selection);
  assert.deepEqual(f.requests.slice(before), []);
  await voice.stop(call.callId);
  f.notification({ method: "thread/realtime/closed", params: { threadId } });
  const next = f.admit(), afterClose = f.requests.length;
  assert.equal(await voice.prepare(next, 0, selection), threadId);
  assert.deepEqual(f.requests.slice(afterClose), []);
  await voice.stop(next.callId);
  f.notification({ method: "thread/realtime/error", params: { threadId, message: "fixture" } });
  const afterError = f.requests.length;
  assert.equal(await voice.prepare(f.admit(), 0, selection), threadId);
  assert.ok(f.requests.slice(afterError).some(value => value.method === "thread/read"));
  assert.ok(f.requests.slice(afterError).some(value => value.method === "thread/settings/update"));
});

test("Incoming and MCP callers share one unused fully prepared task", async (t) => {
  const f = fixture(t), voice = f.voice();
  await voice.prewarm("incoming-user", selection);
  const before = f.requests.length;
  const call = f.admit("mcp-agent");
  await voice.prepare(call, 0, selection);
  assert.deepEqual(f.requests.slice(before), []);
  assert.equal(f.requests.filter(value => value.method === "turn/start").length, 1);
});

test("A call arriving during background model preparation awaits that same task", async (t) => {
  const f = fixture(t), voice = f.voice();
  let release!: () => void;
  f.pausePreparation(new Promise<void>(resolve => { release = resolve; }));
  const warm = voice.prewarm("incoming-user", selection);
  for (let i = 0; i < 100 && !f.requests.some(x => x.method === "turn/start"); i++)
    await new Promise(resolve => setTimeout(resolve, 1));
  assert.ok(f.requests.some(x => x.method === "turn/start"));
  const prepared = voice.prepare(f.admit("mcp-agent"), 0, selection);
  release();
  await warm; await prepared;
  assert.equal(f.requests.filter(value => value.method === "thread/start").length, 1);
  assert.equal(f.requests.filter(value => value.method === "turn/start").length, 1);
});

test("A lost preparation acknowledgement reconciles its original message without repeating the turn", async (t) => {
  const f = fixture(t), first = f.voice();
  f.losePreparationReply();
  await assert.rejects(first.prewarm("incoming-user", selection), /Preparation acknowledgement lost/);
  await first.close();
  const second = f.voice();
  await second.prewarm("mcp-agent", selection);
  assert.equal(f.requests.filter(value => value.method === "thread/start").length, 1);
  assert.equal(f.requests.filter(value => value.method === "turn/start").length, 1);
  assert.ok(f.requests.some(value => value.method === "thread/read" && value.params["includeTurns"] === true));
});

test("A completed conversation keeps its history and the next call uses a fresh prepared task", async (t) => {
  const f = fixture(t), voice = f.voice(), first = f.admit();
  const oldThread = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, oldThread, "First call context", "offer", () => {});
  await voice.stop(first.callId);
  f.notification({ method: "thread/realtime/closed", params: { threadId: oldThread } });
  const second = f.admit(), nextThread = await voice.prepare(second, 0, selection);
  assert.notEqual(nextThread, oldThread);
  assert.equal(f.journal.voiceTask(first.callId, 0), oldThread);
  assert.equal(f.requests.filter(value => value.method === "thread/start").length, 2);
  assert.equal(await voice.start(second, 0, nextThread, "Independent second context", "offer", () => {}), "remote answer");
});

test("A confirmed dead cached agent loop is replaced without erasing its retained call identity", async (t) => {
  const f = fixture(t), first = f.voice();
  await first.prewarm("principal", selection);
  const oldCall = f.admit(), oldThread = await first.prepare(oldCall, 0, selection);
  await first.stop(oldCall.callId);
  await first.close(); f.killLoops();
  const next = f.voice(), call = f.admit();
  const threadId = await next.prepare(call, 0, selection);
  assert.notEqual(threadId, oldThread);
  assert.equal(f.requests.filter(value => value.method === "thread/start").length, 2);
  assert.equal(f.journal.voiceTask(oldCall.callId, 0), oldThread);
  assert.equal(f.journal.voiceTask(call.callId, 0), threadId);
  assert.equal(await next.start(call, 0, threadId, "New call context", "local offer", () => {}), "remote answer");
});

test("Cached native tasks survive a client reopen, retain their project and receive each full initial prompt", async (t) => {
  const f = fixture(t),
    first = f.voice();
  await first.prewarm("principal", selection);
  await first.close();
  const second = f.voice(),
    call = f.admit(),
    threadId = await second.prepare(call, 0, selection);
  assert.equal(
    f.requests.filter((value) => value.method === "thread/start").length,
    1,
  );
  assert.deepEqual(
    f.requests
      .filter((value) => value.method === "thread/resume")
      .map((value) => value.params["threadId"]),
    [threadId],
  );
  assert.equal(
    f.requests.find((value) => value.method === "thread/start")!.params[
      "projectId"
    ],
    f.settings.projectId,
  );
  const prompt = "Complete context. ".repeat(500) + "END";
  assert.equal(
    await second.start(call, 0, threadId, prompt, "local offer", () => {}),
    "remote answer",
  );
  const started = f.requests.find(
    (value) => value.method === "thread/realtime/start",
  )!.params;
  assert.deepEqual(started["initialItems"], [{ role: "user", text: prompt }]);
  assert.ok(
    String(started["realtimeStartInstructions"]).endsWith(
      JSON.stringify(prompt),
    ),
  );
  assert.equal(started["clientManagedHandoffs"], false);
  assert.equal(started["includeStartupContext"], false);
  await second.stop(call.callId);
  assert.equal(
    f.journal.callCommand(call.callId, "call.stopVoice")?.receipt?.ok,
    true,
  );
});

test("Lost control transport reconnects to stop the original task, and close also confirms its stop", async (t) => {
  const f = fixture(t),
    voice = f.voice(),
    call = f.admit(),
    threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, "Hello.", "local offer", () => {});
  f.disconnect();
  await voice.stop(call.callId);
  assert.equal(f.opens(), 2);
  assert.deepEqual(
    f.requests.filter((value) => value.method === "thread/realtime/stop").at(-1)
      ?.params,
    { threadId },
  );
  const next = f.admit(),
    nextThread = await voice.prepare(next, 1, selection);
  await voice.start(
    next,
    1,
    nextThread,
    "Second session.",
    "local offer",
    () => {},
  );
  await voice.close();
  assert.equal(
    f.journal.callCommand(next.callId, "call.stopVoice", 1)?.receipt?.ok,
    true,
  );
});

test("Overlapping calls claim separate tasks without stopping the first context", async (t) => {
  const f = fixture(t),
    voice = f.voice(),
    first = f.admit();
  const threadId = await voice.prepare(first, 0, selection);
  const other = f.admit();
  const nextThread = await voice.prepare(other, 0, selection);
  assert.notEqual(nextThread, threadId);
  assert.equal(
    f.requests.filter((value) => value.method === "thread/start").length,
    2,
  );
  assert.equal(f.journal.voiceTask(first.callId, 0), threadId);
  assert.equal(f.journal.voiceTask(other.callId, 0), nextThread);
  assert.equal(f.requests.filter(value => value.method === "thread/realtime/stop").length, 0);
});

test("Stopping a call releases a prepared replacement after a cancelled restart", async (t) => {
  const f = fixture(t),
    voice = f.voice(),
    call = f.admit();
  const original = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, original, "Hello.", "local offer", () => {});
  const replacement = await voice.prepare(call, 1, selection);
  await voice.stop(call.callId);
  const next = f.admit();
  assert.equal(await voice.prepare(next, 0, selection), replacement);
});

test("A task confirmed missing by native resume is replaced once after reopening the client", async (t) => {
  const f = fixture(t),
    first = f.voice();
  await first.prewarm("principal", selection);
  await first.close();
  f.forgetTasks();
  const second = f.voice(),
    call = f.admit(),
    threadId = await second.prepare(call, 0, selection);
  assert.equal(
    f.requests.filter((value) => value.method === "thread/start").length,
    2,
  );
  assert.equal(f.journal.voiceTask(call.callId, 0), threadId);
});

test("Preparing an unloaded task resumes its recorded identity without another creation", async (t) => {
  const f = fixture(t);
  f.settings.keepTaskLoaded = false;
  const voice = f.voice();
  await voice.prewarm("principal", selection);
  assert.equal(
    f.requests.filter((value) => value.method === "thread/unsubscribe").length,
    1,
  );
  const call = f.admit();
  await voice.prepare(call, 0, selection);
  assert.equal(
    f.requests.filter((value) => value.method === "thread/resume").length,
    1,
  );
  assert.equal(
    f.requests.filter((value) => value.method === "thread/start").length,
    1,
  );
});

test("A call arriving during idle unsubscribe waits and resumes before starting Voice", async (t) => {
  const f = fixture(t);
  f.settings.keepTaskLoaded = false;
  let release!: () => void;
  f.pauseUnload(
    new Promise<void>((resolve) => {
      release = resolve;
    }),
  );
  const voice = f.voice(),
    warm = voice.prewarm("principal", selection);
  for (
    let i = 0;
    i < 100 && !f.requests.some((x) => x.method === "thread/unsubscribe");
    i++
  )
    await new Promise((r) => setTimeout(r, 1));
  assert.ok(f.requests.some((x) => x.method === "thread/unsubscribe"));
  const call = f.admit(),
    prepared = voice.prepare(call, 0, selection);
  release();
  await warm;
  const threadId = await prepared;
  assert.equal(f.requests.filter((x) => x.method === "thread/start").length, 1);
  assert.equal(
    f.requests.filter((x) => x.method === "thread/resume").length,
    1,
  );
  assert.equal(f.journal.voiceTask(call.callId, 0), threadId);
});
