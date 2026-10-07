import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import catalog from "../specs/native/codex-0.159.2/catalog.json" with { type: "json" };
import { PhoneCodexVoice } from "../services/phone-bridge/src/runtime/codex-voice.js";
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
    incoming: [],
    recipients: [{ id: "recipient", destination: "sip:recipient@127.0.0.1" }],
  });
  const settings = {
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
  let unloadPause: Promise<void> | undefined;
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
  const admit = () =>
    journal.admitCall(
      policy.outgoing(epoch, randomUUID(), "principal", "recipient", "voice"),
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
    settings,
    opens: () => opens,
    forgetTasks: () => threads.clear(),
    pauseUnload: (pause: Promise<void>) => {
      unloadPause = pause;
    },
    disconnect: () => {
      online = false;
    },
    notification: (value: NativeNotification) => notification(value),
  };
}

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

test("One cached task cannot be reserved by two concurrent calls before realtime starts", async (t) => {
  const f = fixture(t),
    voice = f.voice(),
    first = f.admit();
  const threadId = await voice.prepare(first, 0, selection);
  const other = f.admit();
  await assert.rejects(voice.prepare(other, 0, selection), {
    code: "phone_voice_task_busy",
  });
  assert.equal(
    f.requests.filter((value) => value.method === "thread/start").length,
    1,
  );
  assert.equal(f.journal.voiceTask(first.callId, 0), threadId);
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
