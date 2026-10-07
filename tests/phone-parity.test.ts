import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PhoneAdmission } from "../services/phone-bridge/src/runtime/admission.js";
import { PhoneJournal } from "../services/phone-bridge/src/runtime/journal.js";
import { synthesizeAnnouncement } from "../services/phone-bridge/src/runtime/speech.js";
import { PhoneCallLogs } from "../services/phone-bridge/src/runtime/call-logs.js";
import { canonical, digest } from "../packages/contracts/src/canonical.js";
import type { PhoneJournalIntent } from "../services/phone-bridge/src/runtime/journal.js";
import type { PhoneStatus } from "../services/phone-bridge/src/runtime/native.js";
import { DatabaseSync } from "node:sqlite";
import type { PhoneIntent } from "../services/phone-bridge/src/runtime/native.js";

test("Voice generation receipts remain independent, reject duplicates and survive archival", (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-generations-")),
    owner = { hostId: "test", serviceNodeId: "phone" };
  let journal = new PhoneJournal(root, owner);
  t.after(() => {
    journal.close();
    rmSync(root, { recursive: true, force: true });
  });
  const epoch = randomUUID();
  journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({
    recipients: [{ id: "main", destination: "sip:fixture@test" }],
    incoming: [],
  });
  const call = journal.admitCall(
    policy.outgoing(epoch, randomUUID(), "main", "main"),
  );
  const originals: PhoneIntent[] = [];
  for (const [method, generation] of [
    ["call.desktop.pauseVoice", 0],
    ["call.desktop.resumeVoice", 1],
    ["call.desktop.pauseVoice", 1],
    ["call.desktop.resumeVoice", 2],
  ] as const) {
    const intent: PhoneIntent = {
      epoch,
      callId: call.callId,
      operationId: randomUUID(),
      method,
      voiceGeneration: generation,
      requestHash: digest(method + generation),
    };
    journal.submit(intent);
    journal.finish(intent, {
      version: 1,
      epoch,
      requestId: 1,
      ok: true,
      error: null,
      result: { state: "confirmed" },
    });
    originals.push(intent);
    assert.throws(
      () => journal.submit({ ...intent, operationId: randomUUID() }),
      { code: "phone_call_conflict" },
    );
  }
  assert.throws(
    () =>
      journal.submit({
        ...originals[0]!,
        method: "call.dial",
        operationId: randomUUID(),
      }),
    { code: "invalid_arguments" },
  );
  const selections = [1, 2].map((commandSequence) => {
    const args = {
      threadId: randomUUID(),
      prompt: "Update selection",
      model: "gpt-6-sol",
      reasoningEffort: "high",
      commandSequence,
    };
    const intent: PhoneJournalIntent & { method: "call.selectVoice" } = {
      epoch,
      callId: call.callId,
      operationId: randomUUID(),
      method: "call.selectVoice",
      ...args,
      requestHash: digest(canonical(args)),
    };
    journal.submit(intent);
    journal.finishVoiceSelection(
      intent,
      commandSequence === 1 ? "sent" : "outcome_unknown",
    );
    return journal.voiceSelectionCommand(call.callId, commandSequence);
  });
  const release: PhoneIntent = {
    epoch,
    callId: call.callId,
    operationId: randomUUID(),
    method: "call.release",
    requestHash: digest("release"),
  };
  journal.submit(release);
  journal.finish(release, {
    version: 1,
    epoch,
    requestId: 1,
    ok: true,
    error: null,
    result: { callId: call.callId, released: true },
  });
  journal.releaseCall(call.callId, release.operationId);
  journal.submit({
    epoch,
    callId: null,
    operationId: randomUUID(),
    method: "registration.reconnect",
    requestHash: digest("next"),
  });
  assert.ok(journal.archiveStatus().records > 0);
  journal.close();
  journal = new PhoneJournal(root, owner);
  for (const intent of originals)
    assert.deepEqual(
      journal.callCommand(call.callId, intent.method, intent.voiceGeneration)
        ?.intent,
      intent,
    );
  assert.deepEqual(
    [1, 2].map((sequence) =>
      journal.voiceSelectionCommand(call.callId, sequence),
    ),
    selections,
    "every keypad selection remains addressable after compaction and reopen, including an unknown outcome",
  );
});

test("unsupported retained format is refused without implicit migration", (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-migration-"));
  const owner = { hostId: "test", serviceNodeId: "phone" };
  let journal = new PhoneJournal(root, owner);
  t.after(() => {
    journal.close();
    rmSync(root, { recursive: true, force: true });
  });
  const epoch = randomUUID();
  journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({
    recipients: [{ id: "main", destination: "sip:fixture@test" }],
    incoming: [],
  });
  const call = journal.admitCall(
    policy.outgoing(epoch, randomUUID(), "main", "main"),
  );
  journal.close();
  const db = new DatabaseSync(join(root, "phone-commands.sqlite"));
  db.prepare("INSERT OR REPLACE INTO meta VALUES (?,?)").run(
    "active_call",
    call.callId,
  );
  db.exec("DROP TABLE active_calls; PRAGMA user_version=5;");
  db.close();
  assert.throws(() => new PhoneJournal(root, owner, undefined, 2), {
    code: "unsupported_storage",
  });
  const retained = new DatabaseSync(join(root, "phone-commands.sqlite"));
  assert.equal(
    retained.prepare("PRAGMA user_version").get()!["user_version"],
    5,
  );
  retained.close();
});

for (const failure of ["network", "http"] as const) {
  test(`speech ${failure} failure releases resources, stays fenced after reopening and never exposes provider secrets`, async (t) => {
    const root = mkdtempSync(join(tmpdir(), "ivy-phone-speech-failure-")),
      owner = { hostId: "test", serviceNodeId: "phone" };
    let journal = new PhoneJournal(root, owner);
    t.after(() => {
      journal.close();
      rmSync(root, { recursive: true, force: true });
    });
    const epoch = randomUUID();
    journal.beginEpoch(epoch);
    const policy = new PhoneAdmission({
      recipients: [{ id: "main", destination: "sip:fixture@test" }],
      incoming: [],
    });
    const call = journal.admitCall(
      policy.outgoing(epoch, randomUUID(), "main", "main", "windows", {
        announcement: "Test",
        repeatCount: 1,
        timeoutSeconds: 10,
      }),
    );
    const credentialsPath = join(root, "speech.json");
    writeFileSync(
      credentialsPath,
      JSON.stringify({
        schemaVersion: 1,
        speech: { apiKey: "protected-fixture" },
      }),
    );
    const settings = {
      credentialsPath,
      endpoint: "https://fixture.test/speech",
      model: "fixture",
      voice: "fixture",
    };
    let effects = 0,
      cancelled = false;
    const fetcher: typeof fetch = async () => {
      effects++;
      if (failure === "network")
        throw new Error("provider echoed protected-fixture");
      return new Response(
        new ReadableStream<Uint8Array>({
          start(controller) {
            controller.enqueue(
              Buffer.from("provider echoed protected-fixture"),
            );
          },
          cancel() {
            cancelled = true;
          },
        }),
        { status: 503 },
      );
    };
    await assert.rejects(
      synthesizeAnnouncement(journal, call, "Test", settings, fetcher),
      { code: "phone_speech_unknown" },
    );
    assert.equal(
      cancelled,
      failure === "http",
      "an HTTP error response is cancelled without consuming its body",
    );
    journal.close();
    journal = new PhoneJournal(root, owner);
    await assert.rejects(
      synthesizeAnnouncement(journal, call, "Test", settings, fetcher),
      { code: "phone_speech_unknown" },
    );
    assert.equal(effects, 1);
    assert.ok(
      !JSON.stringify(
        journal.callCommand(call.callId, "call.screening.synthesize"),
      ).includes("protected-fixture"),
    );
  });
}

test("direct destinations require explicit service configuration, Windows route and valid E.164/SIP syntax", () => {
  const policy = new PhoneAdmission({
    recipients: [],
    incoming: [],
    allowDirectDial: true,
    dialDomain: "sip.example.test",
  });
  const epoch = randomUUID(),
    operation = randomUUID();
  assert.equal(
    policy.outgoing(
      epoch,
      operation,
      "main",
      null,
      "windows",
      undefined,
      "+49123456789",
    ).destination,
    "sip:+49123456789@sip.example.test",
  );
  assert.equal(
    policy.outgoing(
      epoch,
      operation,
      "other",
      null,
      "windows",
      undefined,
      "+49123456789",
    ).destination,
    "sip:+49123456789@sip.example.test",
  );
  assert.throws(() =>
    policy.outgoing(
      epoch,
      operation,
      "main",
      null,
      "voice",
      undefined,
      "+49123456789",
    ),
  );
  assert.throws(() =>
    policy.outgoing(
      epoch,
      operation,
      "main",
      null,
      "windows",
      undefined,
      "sip:user@test\r\nInjected: yes",
    ),
  );
});

test("concurrent call admissions retain independent owners and their configured capacity across reopen", (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-multi-"));
  let journal = new PhoneJournal(
    root,
    { hostId: "test", serviceNodeId: "phone" },
    undefined,
    3,
  );
  t.after(() => {
    journal.close();
    rmSync(root, { recursive: true, force: true });
  });
  const epoch = randomUUID();
  journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({
    recipients: [{ id: "main", destination: "sip:fixture@test" }],
    incoming: [],
  });
  const first = journal.admitCall(
    policy.outgoing(epoch, randomUUID(), "main", "main", "windows"),
  );
  const voice = journal.admitCall(
    policy.outgoing(epoch, randomUUID(), "main", "main", "voice"),
  );
  const last = journal.admitCall(
    policy.outgoing(epoch, randomUUID(), "other", "main", "voice"),
  );
  assert.throws(() =>
    journal.admitCall(
      policy.outgoing(epoch, randomUUID(), "main", "main", "windows"),
    ),
  );
  journal.close();
  journal = new PhoneJournal(
    root,
    { hostId: "test", serviceNodeId: "phone" },
    undefined,
    3,
  );
  assert.equal(journal.currentCalls().length, 3);
  journal.discardUnpreparedCall(first.callId);
  assert.equal(journal.currentCall(first.callId), null);
  assert.equal(journal.currentCall(voice.callId)?.callId, voice.callId);
  assert.equal(journal.currentCall(last.callId)?.callId, last.callId);
});

test("synthesis retains one original result, checks WAV header and rejects changed requests", async (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-parity-"));
  const journal = new PhoneJournal(root, {
    hostId: "test",
    serviceNodeId: "phone",
  });
  t.after(() => {
    journal.close();
    rmSync(root, { recursive: true, force: true });
  });
  const epoch = randomUUID();
  journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({
    incoming: [],
    recipients: [{ id: "main", destination: "sip:fixture@test" }],
  });
  const call = journal.admitCall(
    policy.outgoing(epoch, randomUUID(), "main", "main", "windows", {
      announcement: "Automatic test call",
      repeatCount: 1,
      timeoutSeconds: 10,
    }),
  );
  const credentialsPath = join(root, "speech-fixture.json");
  writeFileSync(
    credentialsPath,
    JSON.stringify({
      schemaVersion: 1,
      speech: { apiKey: "synthetic-fixture-key" },
    }),
  );
  const settings = {
    credentialsPath,
    endpoint: "https://speech.example.test/v1/audio/speech",
    model: "fixture",
    voice: "fixture",
  };
  const wav = Buffer.alloc(48);
  wav.write("RIFF");
  wav.write("WAVE", 8);
  let effects = 0;
  const fetcher: typeof fetch = async (_url, args) => {
    effects++;
    assert.equal(args?.redirect, "error");
    assert.match(String(args?.body), /response_format.*wav/);
    return new Response(wav);
  };
  const result = await synthesizeAnnouncement(
    journal,
    call,
    "Automatic test call",
    settings,
    fetcher,
  );
  assert.equal(result.sha256, digest(readFileSync(result.wavPath)));
  assert.deepEqual(
    await synthesizeAnnouncement(
      journal,
      call,
      "Automatic test call",
      settings,
      fetcher,
    ),
    result,
  );
  assert.equal(effects, 1);
  await assert.rejects(
    synthesizeAnnouncement(journal, call, "Changed request", settings, fetcher),
  );
  assert.ok(
    !JSON.stringify(
      journal.callCommand(call.callId, "call.screening.synthesize"),
    ).includes("synthetic-fixture-key"),
  );
});

test("diagnostic snapshots survive reopen, deduplicate and isolate calls", (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-logs-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  let logs = new PhoneCallLogs(root);
  const policy = new PhoneAdmission({
    recipients: [{ id: "main", destination: "sip:fixture@test" }],
    incoming: [],
  });
  const call = {
    ...policy.outgoing(randomUUID(), randomUUID(), "main", "main"),
    callId: randomUUID(),
  };
  const observed: PhoneStatus = {
    configured: true,
    registration: null,
    endpoint: "127.0.0.1:5060",
    media: null,
    call: {
      id: call.callId,
      direction: "outgoing",
      state: "connected",
      sipCallId: "wire",
      incoming: null,
      error: null,
    },
  };
  logs.record(call, observed);
  logs.record(call, observed);
  logs.close();
  logs = new PhoneCallLogs(root);
  try {
    assert.equal(logs.read(call.callId).length, 1);
    assert.equal(logs.read(randomUUID()).length, 0);
  } finally {
    logs.close();
  }
});
