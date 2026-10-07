import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";
import { PassThrough } from "node:stream";
import { PhoneAdmission } from "../services/phone-bridge/src/runtime/admission.js";
import { PhoneCallCommands } from "../services/phone-bridge/src/runtime/calls.js";
import { PhoneFlow } from "../services/phone-bridge/src/runtime/flow.js";
import type {
  PhoneVoicePort,
  PhoneVoiceSelectionCommand,
} from "../services/phone-bridge/src/runtime/codex-voice.js";
import type { PhoneFlowSettings } from "../services/phone-bridge/src/runtime/flow.js";
import {
  PhoneJournal,
  phoneOutcomeReservation,
} from "../services/phone-bridge/src/runtime/journal.js";
import { PhoneNativeClient } from "../services/phone-bridge/src/runtime/native.js";
import type { PhoneIntent } from "../services/phone-bridge/src/runtime/native.js";
import { PhoneBridge } from "../services/phone-bridge/src/runtime/bridge.js";
import {
  phoneRegistry,
  validatePhoneService,
} from "../services/phone-bridge/src/runtime/registry.js";
import { phoneSettings } from "../services/phone-bridge/src/runtime/settings.js";
import { validateShared } from "../packages/contracts/src/validation.js";
import type { InvocationContext } from "../packages/sdk/src/service.js";
import { canonical, digest, IvyError } from "../packages/sdk/src/node.js";

const desktop = {
  pid: 123,
  startTimeUtcTicks: "1",
  imagePath: "C:\\fixture.exe",
  appUserModelId: "Fixture.Package!UI",
};
const appTools = {
  nodeExecutable: process.execPath,
  nodeExecutableHash: "sha256:" + "0".repeat(64),
  serverPath: "C:\\fixture\\server.mjs",
  serverHash: "sha256:" + "0".repeat(64),
  pipePath: "auto",
  actorThreadId: "00000000-0000-4000-8000-000000000099",
};
const settings: PhoneFlowSettings = {
  outgoingRoute: "voice",
  codexVoice: {
    nativeExecutable: join(tmpdir(), "fixture", "codex.exe"),
    nativeExecutableHash: "sha256:" + "0".repeat(64),
    nativeVersion: "0.159.2",
    codexHome: join(tmpdir(), "fixture", "home"),
    cwd: join(tmpdir(), "fixture", "work"),
    appServer: { mode: "owned-stdio" },
  },
  audio: {
    captureEndpointId: "capture",
    renderEndpointId: "render",
    sourceMode: "system_excluding_runtime",
    captureBufferMs: 20,
    renderLatencyMs: 20,
    queueMs: 40,
  },
  ringSeconds: 30,
};
const dispatch = {
  phase: "submitted",
  method: "hotkey",
  micro: {
    phase: "submitted",
    press: { phase: "submitted", pressed: true, generation: 4294967296 },
    release: { phase: "submitted", pressed: false, generation: 4294967296 },
  },
  hotkey: {
    phase: "submitted",
    requested: 6,
    submitted: 6,
    keyUpSubmitted: true,
    errorCode: null,
  },
};
const tick = () => new Promise<void>((done) => setImmediate(done));
const context = (caller = "main"): InvocationContext => ({
  callerPrincipalId: caller,
  generation: 1,
  signal: new AbortController().signal,
});
async function until(condition: () => boolean) {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    if (condition()) return;
    await new Promise((done) => setTimeout(done, 5));
  }
  assert.fail("Expected original flow did not settle.");
}
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
type Request = {
  method: string;
  requestId: number;
  params: Record<string, unknown>;
};
function fixture(
  t: TestContext,
  maxBytes = 512 * 1024 * 1024,
  maxConcurrentCalls = 1,
  policyDefinition?: ConstructorParameters<typeof PhoneAdmission>[0],
) {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-flow-"));
  const policy = new PhoneAdmission(
    policyDefinition ?? {
      recipients: [{ id: "personal", destination: "sip:personal@127.0.0.1" }],
      incoming: [
        {
          peerAddress: "127.0.0.1",
          transport: "udp",
          fromUri: "sip:known@fixture",
        },
      ],
    },
  );
  const journal = new PhoneJournal(
    root,
    { hostId: "fixture", serviceNodeId: "phone" },
    { maxOperations: 100, maxBytes, maxEpochs: 100 },
    maxConcurrentCalls,
  );
  const epoch = randomUUID();
  journal.beginEpoch(epoch);
  const input = new PassThrough(),
    output = new PassThrough(),
    requests: Request[] = [],
    errors: string[] = [];
  const native = new PhoneNativeClient(epoch, input, output, journal.hooks),
    calls = new PhoneCallCommands(policy, journal, native);
  let activeId: string | null = null,
    direction = "outgoing",
    state = "prepared",
    incoming: Record<string, unknown> | null = null;
  let desktopWaitingReads = 0;
  let intercept: ((request: Request) => Promise<void>) | null = null;
  let startState = "active",
    stopState = "stopped",
    launchPhase = "ready",
    credentialReads = 0;
  let captureState: "none" | "matched" | "ambiguous" | "unavailable" = "none";
  let registration: Record<string, unknown> | null = null;
  let screening = "none",
    bridgeFailure = false,
    audioState = "suspended";
  let command: string | null = null,
    commandSequence = 0,
    commandModel: string | null = null,
    commandEffort: string | null = null;
  const commands: {
    command: "new_voice" | "select_voice";
    sequence: number;
    model: string | null;
    reasoningEffort: string | null;
  }[] = [];
  const observation = () => ({
    id: activeId,
    direction,
    state,
    sipCallId: incoming ? "wire" : null,
    error: null,
    incoming,
    features: {
      access: "trusted",
      disableCodecUpgrade: false,
      screening,
      command,
      commandSequence,
      commandState: command ? "pending" : null,
      model: commandModel,
      reasoningEffort: commandEffort,
      commands,
      commandOverflow: false,
    },
  });
  const respond = async (r: Request) => {
    if (intercept) await intercept(r);
    if (
      r.method === "call.screening.bridge" &&
      (bridgeFailure || state !== "connected")
    ) {
      output.write(
        JSON.stringify({
          version: 1,
          epoch,
          requestId: r.requestId,
          ok: false,
          result: null,
          error: "runtime_not_ready",
        }) + "\n",
      );
      return;
    }
    let result: unknown;
    switch (r.method) {
      case "audio.probe":
        result = {
          state: "completed",
          detected: false,
          peak: 0,
          samples: 480000,
          sampleRate: 48000,
          channels: 2,
          excludedProcessId: 123,
        };
        break;
      case "codec.test":
        result = {
          passed: false,
          codecs: ["G722", "PCMA", "PCMU", "OPUS", "EVS"].map((name) => ({
            name,
            passed: name !== "EVS",
            frames: name === "EVS" ? 0 : 25,
            encodedBytes: 0,
            decodedSamples: 0,
            rms: 0,
            error: name === "EVS" ? "codec_library_missing" : null,
          })),
        };
        break;
      case "inventory":
        result = {
          devices: [
            {
              id: "capture",
              name: "Fixture cable",
              direction: "capture",
              state: "Active",
            },
          ],
          codecs: [
            {
              name: "G722",
              payload: 9,
              pcmRate: 16000,
              rtpClockRate: 8000,
              channels: 1,
            },
          ],
          processLoopbackSupported: true,
        };
        break;
      case "call.realtime.prepare":
        result = {
          callId: activeId,
          generation: r.params["generation"],
          sdp: "fixture offer",
        };
        break;
      case "call.realtime.answer":
        audioState = "open";
        result = { callId: activeId, connected: true };
        break;
      case "call.realtime.stop":
        result = { callId: activeId, stopped: true };
        break;
      case "call.features":
        result = {
          access: "trusted",
          disableCodecUpgrade: false,
          screening: "none",
          command: null,
          commandSequence: 0,
          commands: [],
          commandOverflow: false,
        };
        break;
      case "call.windows.connect":
        result = { callId: activeId, state: "open", route: null };
        break;
      case "call.screening.prepare":
        screening = "waiting";
        result = { prepared: true };
        break;
      case "call.screening.bridge":
        screening = "bridged";
        result = { callId: activeId, state: "open", route: null };
        break;
      case "status":
        result = {
          configured: true,
          endpoint: "127.0.0.1:5060",
          registration,
          micro: { state: "ready", generation: 4294967296, errorCode: null },
          call: activeId ? observation() : null,
          media: activeId
            ? {
                closed: state === "local_ended",
                failed: false,
                sentPackets: 0,
                receivedPackets: 0,
                receive: {
                  queuedPackets: 0,
                  maximumPackets: 8,
                  reorderMs: 20,
                  duplicatePackets: 0,
                  latePackets: 0,
                  overflowPackets: 0,
                  foreignPackets: 0,
                  gatedPackets: 0,
                  missingPackets: 0,
                  maximumResidenceMs: 0,
                },
                audio: { callId: activeId, state: audioState, route: null },
              }
            : null,
        };
        break;
      case "call.prepare":
        activeId = String(r.params["callId"]);
        direction = "outgoing";
        state = "prepared";
        result = observation();
        break;
      case "call.claim":
        assert.equal(r.params["callId"], activeId);
        assert.equal(direction, "incoming");
        result = observation();
        break;
      case "call.desktop.launch":
        result = {
          callId: activeId,
          action: "launch",
          result: {
            phase: launchPhase,
            identity: launchPhase === "ready" ? desktop : null,
            activationPid: launchPhase === "submitted" ? 123 : null,
            errorCode: null,
          },
        };
        break;
      case "desktop.observe":
        result =
          desktopWaitingReads-- > 0
            ? { state: "waiting", identity: null }
            : { state: "ready", identity: desktop };
        break;
      case "call.audio.prepare":
        result = { callId: activeId, state: "suspended", route: null };
        break;
      case "call.audio.rebind":
        audioState = "open";
        result = { callId: activeId, state: "open", route: null };
        break;
      case "call.desktop.startVoice":
        result = {
          callId: r.params["callId"],
          action: "startVoice",
          result: { state: startState, dispatch, reason: null },
        };
        break;
      case "call.desktop.pauseVoice":
        result = {
          callId: r.params["callId"],
          action: "pauseVoice",
          generation: r.params["generation"],
          result: { state: "stopped", dispatch, reason: null },
        };
        break;
      case "call.desktop.resumeVoice":
        result = {
          callId: r.params["callId"],
          action: "resumeVoice",
          generation: r.params["generation"],
          result: { state: "active", dispatch, reason: null },
        };
        break;
      case "call.desktop.stopVoice":
        result = {
          callId: r.params["callId"],
          action: "stopVoice",
          result: { state: stopState, dispatch, reason: null },
        };
        break;
      case "call.dial":
      case "call.answer":
        state = "connected";
        result = observation();
        break;
      case "call.waiting.end":
        result = { callId: activeId, ended: true };
        break;
      case "call.command.feedback":
        result = {
          callId: activeId,
          commandSequence: r.params["commandSequence"],
          success: r.params["success"],
        };
        break;
      case "call.hangup":
        state = "local_ended";
        result = observation();
        break;
      case "call.release":
        result = { callId: activeId, released: true };
        activeId = null;
        break;
      case "desktop.captureOwner":
        result = {
          endpointId: r.params["endpointId"],
          state: captureState,
          identity:
            captureState === "matched"
              ? {
                  instanceId: "capture-instance",
                  pid: 321,
                  processStartTimeUtcTicks: "1",
                }
              : null,
        };
        break;
      default:
        throw new Error("Unexpected fixture request: " + r.method);
    }
    output.write(
      JSON.stringify({
        version: 1,
        epoch,
        requestId: r.requestId,
        ok: true,
        result,
        error: null,
      }) + "\n",
    );
  };
  input.on("data", (bytes) => {
    const r = JSON.parse(String(bytes).trim()) as Request;
    requests.push(r);
    void respond(r).catch((error) => errors.push(String(error)));
  });
  const flows: PhoneFlow[] = [];
  const bound = new Map<string, string>(),
    prompts: { generation: number; threadId: string; prompt: string }[] = [];
  const promptSelections: unknown[] = [];
  const selectionUpdates: {
    threadId: string;
    command: PhoneVoiceSelectionCommand;
    selection: unknown;
  }[] = [];
  let voicePrepares = 0;
  let failPrompt = false,
    failSelection = false,
    promptError = "app_tools_unavailable";
  const events: string[] = [];
  const voicePrewarms: string[] = [];
  const defaultVoiceTasks: PhoneVoicePort = {
    async prewarm(principalId) { voicePrewarms.push(principalId); },
    async prepare(call, generation, selection) {
      voicePrepares++;
      const threadId =
        bound.get(call.principalId + ":" + generation) ?? randomUUID();
      bound.set(call.principalId + ":" + generation, threadId);
      const intent = {
        epoch: call.epoch,
        callId: call.callId,
        operationId: randomUUID(),
        method: "call.bindVoice" as const,
        voiceGeneration: generation,
        threadId,
        requestHash: "sha256:" + "0".repeat(64),
      };
      journal.submit(intent);
      journal.finishVoiceBinding(intent);
      return threadId;
    },
    async start(call, generation, threadId, prompt, _sdp, guard) {
      guard();
      events.push("start");
      prompts.push({ generation, threadId, prompt });
      const intent = {
        epoch: call.epoch,
        callId: call.callId,
        operationId: randomUUID(),
        method: "call.promptVoice" as const,
        voiceGeneration: generation,
        threadId,
        prompt,
        requestHash: "sha256:" + "0".repeat(64),
      };
      journal.submit(intent);
      journal.finishVoicePrompt(
        intent,
        failPrompt ? "outcome_unknown" : "sent",
      );
      if (failPrompt) throw new IvyError(promptError, "Voice startup failed.");
      return "fixture answer";
    },
    async prompt(call, threadId, prompt, operationId, requestHash, guard) {
      await guard();
      events.push("prompt");
      prompts.push({
        generation: journal.latestVoiceGeneration(call.callId)!,
        threadId,
        prompt,
      });
      const intent = {
        epoch: call.epoch,
        callId: call.callId,
        operationId,
        method: "call.forwardVoice" as const,
        threadId,
        prompt,
        requestHash,
      };
      journal.submit(intent);
      journal.finishVoicePrompt(
        intent,
        failPrompt ? "outcome_unknown" : "sent",
      );
      if (failPrompt) throw new IvyError(promptError, "Voice prompt failed.");
    },
    async select(call, threadId, command, selection, guard) {
      await guard();
      if (failSelection)
        throw new IvyError("phone_voice_unavailable", "Selection failed.");
      selectionUpdates.push({ threadId, command, selection });
      const values = {
        threadId,
        prompt: "Voice task model selection",
        ...selection,
        ...("commandSequence" in command
          ? { commandSequence: command.commandSequence }
          : {}),
      };
      const intent = {
        epoch: call.epoch,
        callId: call.callId,
        operationId:
          "operationId" in command ? command.operationId : randomUUID(),
        method: "call.selectVoice" as const,
        ...values,
        requestHash: digest(canonical(values)),
      };
      journal.submit(intent);
      journal.finishVoiceSelection(intent, "sent");
    },
    failed() {
      return false;
    },
    async stop() {
      events.push("stop");
    },
    async close() {
      events.push("close");
    },
  };
  const flow = (
    value: PhoneFlowSettings = settings,
    voice: PhoneVoicePort = defaultVoiceTasks,
  ) => {
    const f = new PhoneFlow(
      calls,
      native,
      value,
      async () => {
        credentialReads++;
        return { username: "fixture", password: "never-retained" };
      },
      (_id, code) => errors.push(code),
      undefined,
      voice,
    );
    flows.push(f);
    return f;
  };
  t.after(async () => {
    for (const f of flows) await f.close().catch(() => undefined);
    native.close();
    input.destroy();
    output.destroy();
    journal.close();
    assert.ok(relative(tmpdir(), root).startsWith("ivy-phone-flow-"));
    rmSync(root, { recursive: true, force: true });
  });
  return {
    journal,
    native,
    calls,
    flow,
    requests,
    errors,
    epoch,
    root,
    events,
    voicePrewarms,
    port: defaultVoiceTasks,
    screening: (value: string, fail = false) => {
      screening = value;
      bridgeFailure = fail;
    },
    registration: (value: Record<string, unknown> | null) => {
      registration = value;
    },
    methods: () => requests.map((r) => r.method),
    credentialReads: () => credentialReads,
    intercept: (value: (r: Request) => Promise<void>) => {
      intercept = value;
    },
    voice: (start: string, stop: string) => {
      startState = start;
      stopState = stop;
    },
    capture: (value: "none" | "matched" | "ambiguous" | "unavailable") => {
      captureState = value;
    },
    audio: (value: string) => {
      audioState = value;
    },
    launch: (phase: string) => {
      launchPhase = phase;
    },
    desktopWaiting: (reads: number) => {
      desktopWaitingReads = reads;
    },
    voicePrepares: () => voicePrepares,
    failPrompt: (value: boolean, code = "app_tools_unavailable") => {
      failPrompt = value;
      promptError = code;
    },
    failSelection: (value: boolean) => {
      failSelection = value;
    },
    ended: () => {
      state = "local_ended";
    },
    prompts,
    promptSelections,
    selectionUpdates,
    activate: (callId: string) => {
      activeId = callId;
      direction = "outgoing";
      state = "prepared";
    },
    command: (
      value: string | null,
      sequence: number,
      model: string | null = null,
      effort: string | null = null,
    ) => {
      command = value;
      commandSequence = sequence;
      commandModel = model;
      commandEffort = effort;
      if (value === "new_voice" || value === "select_voice")
        commands.push({
          command: value,
          sequence,
          model,
          reasoningEffort: effort,
        });
    },
    incoming: () => {
      activeId = randomUUID();
      direction = "incoming";
      state = "ringing";
      incoming = {
        sipCallId: "wire",
        fromUri: "sip:known@fixture",
        peerAddress: "127.0.0.1",
        peerPort: 5060,
        transport: "udp",
      };
      return activeId;
    },
  };
}

test("Voice connects native WebRTC, delivers a complete long prompt and reuses its task on a later call", async (t) => {
  const f = fixture(t),
    flow = f.flow(),
    prompt = "Call context. ".repeat(900) + "Reference: full-prompt-end";
  const first = await flow.request(
    "main",
    randomUUID(),
    "personal",
    "voice",
    undefined,
    prompt,
  );
  await until(
    () =>
      !!f.journal.callCommand(first.callId, "call.realtime.answer")?.receipt,
  );
  assert.equal(f.prompts[0]!.prompt, prompt);
  const threadId = f.prompts[0]!.threadId;
  assert.ok(
    !f.methods().some((method) => /desktop|call\.audio\./.test(method)),
  );
  await flow.hangup("main", first.callId);
  assert.equal(f.journal.currentCall(), null);
  const second = await flow.request(
    "main",
    randomUUID(),
    "personal",
    "voice",
    undefined,
    "A different initial prompt.",
  );
  await until(
    () =>
      !!f.journal.callCommand(second.callId, "call.realtime.answer")?.receipt,
  );
  assert.equal(f.prompts[1]!.threadId, threadId);
  assert.equal(f.prompts[1]!.prompt, "A different initial prompt.");
});

test("Voice task and native offer preparation overlap SIP dialing, but initial speech waits for answer", async (t) => {
  const f = fixture(t),
    flow = f.flow(),
    dialing = deferred();
  f.intercept(async (r) => {
    if (r.method === "call.dial") {
      assert.equal(r.params["waiting"], false);
      await dialing.promise;
    }
  });
  const call = await flow.request("main", randomUUID(), "personal", "voice");
  await until(
    () =>
      f.voicePrepares() === 1 && f.methods().includes("call.realtime.prepare"),
  );
  assert.equal(f.prompts.length, 0);
  dialing.resolve();
  await until(
    () => !!f.journal.callCommand(call.callId, "call.realtime.answer")?.receipt,
  );
  assert.ok(!f.methods().includes("call.waiting.end"));
});

test("Incoming Voice uses the configured initial prompt after SIP answer and leaves no native peer after hangup", async (t) => {
  const f = fixture(t),
    flow = f.flow({
      ...settings,
      incomingInitialPrompt: "Discuss the supplied appointment.",
    });
  f.intercept(async (r) => {
    if (r.method === "call.answer") assert.equal(r.params["waiting"], false);
  });
  const callId = f.incoming();
  await flow.accept("main", randomUUID(), callId);
  await until(
    () => !!f.journal.callCommand(callId, "call.realtime.answer")?.receipt,
  );
  assert.equal(f.prompts[0]!.prompt, "Discuss the supplied appointment.");
  assert.ok(
    !f.methods().includes("call.prepare") && !f.methods().includes("call.dial"),
  );
  await flow.hangup("main", callId);
  assert.deepEqual(f.voicePrewarms, ["main"]);
  assert.ok(
    f.methods().indexOf("call.hangup") <
      f.methods().indexOf("call.realtime.stop"),
  );
  assert.equal(f.journal.currentCall(), null);
});

test("A hangup during task preparation prevents realtime startup and revokes SIP before waiting for Codex", async (t) => {
  const f = fixture(t),
    gate = deferred();
  const flow = f.flow(settings, {
    ...f.port,
    async prepare(...args) {
      await gate.promise;
      return f.port.prepare(...args);
    },
  });
  const call = await flow.request("main", randomUUID(), "personal", "voice");
  await until(() => f.methods().includes("call.realtime.prepare"));
  await flow.hangup("main", call.callId);
  assert.ok(f.methods().includes("call.hangup"));
  gate.resolve();
  await tick();
  await tick();
  assert.equal(f.prompts.length, 0);
  assert.ok(!f.methods().includes("call.realtime.answer"));
});

test("Forwarded Voice prompts preserve the existing SIP call and their original operation on replay", async (t) => {
  const f = fixture(t),
    flow = f.flow();
  const call = await flow.request(
    "main",
    randomUUID(),
    "personal",
    "voice",
    undefined,
    "First context.",
  );
  await until(
    () => !!f.journal.callCommand(call.callId, "call.realtime.answer")?.receipt,
  );
  await tick();
  const operationId = randomUUID();
  const forwarded = await flow.request(
    "main",
    operationId,
    "personal",
    "voice",
    undefined,
    "Additional context.",
  );
  assert.equal(forwarded.callId, call.callId);
  await flow.request(
    "main",
    operationId,
    "personal",
    "voice",
    undefined,
    "Additional context.",
  );
  assert.equal(f.prompts.length, 2);
  assert.equal(
    f.methods().filter((method) => method === "call.dial").length,
    1,
  );
  await assert.rejects(
    async () =>
      flow.request(
        "main",
        operationId,
        "personal",
        "voice",
        undefined,
        "Changed context.",
      ),
    { code: "mutation_conflict" },
  );
});

test("Voice selection retains the task and restart binds a new native generation without redialing", async (t) => {
  const f = fixture(t),
    flow = f.flow();
  const call = await flow.request("main", randomUUID(), "personal", "voice");
  await until(
    () => !!f.journal.callCommand(call.callId, "call.realtime.answer")?.receipt,
  );
  await tick();
  const before = f.prompts[0]!.threadId;
  const selected = await flow.controlVoice("main", call.callId, randomUUID(), {
    action: "select",
    selection: { model: "gpt-6-astra", reasoningEffort: "high" },
  });
  assert.equal(selected.phase, "result");
  assert.equal(f.selectionUpdates[0]!.threadId, before);
  const operationId = randomUUID();
  const restarted = await flow.controlVoice("main", call.callId, operationId, {
    action: "restart",
  });
  assert.equal(restarted.receipt?.ok, true);
  assert.notEqual(f.prompts[1]!.threadId, before);
  assert.equal(f.prompts[1]!.generation, 1);
  await flow.controlVoice("main", call.callId, operationId, {
    action: "restart",
  });
  assert.equal(f.prompts.length, 2);
  assert.equal(
    f.methods().filter((method) => method === "call.dial").length,
    1,
  );
});

test("Confirmed reasoning is remembered across calls and restart, while disabled memory keeps the default", async (t) => {
  const f = fixture(t), configured = { ...settings, codexVoice: { ...settings.codexVoice!, rememberReasoning: true, queueMs: 160, playbackPrebufferMs: 100 } };
  const flow = f.flow(configured), call = await flow.request("main", randomUUID(), "personal", "voice");
  await until(() => !!f.journal.callCommand(call.callId, "call.realtime.answer")?.receipt);
  await tick();
  await flow.controlVoice("main", call.callId, randomUUID(), { action: "select",
    selection: { model: "gpt-6-astra", reasoningEffort: "xhigh" } });
  assert.equal(f.journal.rememberedVoiceReasoning(), "xhigh");
  const prepare = f.requests.find(x => x.method === "call.realtime.prepare")!;
  assert.equal(prepare.params["queueMs"], 160);
  assert.equal(prepare.params["playbackPrebufferMs"], 100);
  await flow.hangup("main", call.callId);
  const restarted = f.flow(configured);
  assert.deepEqual(restarted.voiceSelection(randomUUID()), { model: "gpt-6-sol", reasoningEffort: "xhigh" });
  assert.deepEqual(f.flow(settings).voiceSelection(randomUUID()), { model: "gpt-6-sol", reasoningEffort: "high" });
});

test("An unconfirmed reasoning change cannot replace the remembered effort", async (t) => {
  const f = fixture(t), flow = f.flow({ ...settings, codexVoice: { ...settings.codexVoice!, rememberReasoning: true } });
  f.journal.rememberVoiceReasoning("medium");
  const call = await flow.request("main", randomUUID(), "personal", "voice");
  await until(() => !!f.journal.callCommand(call.callId, "call.realtime.answer")?.receipt);
  await tick();
  f.failSelection(true);
  await assert.rejects(flow.controlVoice("main", call.callId, randomUUID(), { action: "select",
    selection: { model: "gpt-6-sol", reasoningEffort: "max" } }));
  assert.equal(f.journal.rememberedVoiceReasoning(), "medium");
});

test("A rejected initial Voice prompt closes the original SIP call and cleans its prepared native peer", async (t) => {
  const f = fixture(t),
    flow = f.flow();
  f.failPrompt(true, "phone_voice_unavailable");
  const call = await flow.request("main", randomUUID(), "personal", "voice");
  await until(() => f.journal.currentCall(call.callId) === null);
  assert.ok(f.errors.includes("phone_voice_unavailable"));
  assert.ok(f.methods().includes("call.realtime.stop"));
  assert.ok(!f.methods().includes("call.realtime.answer"));
});

test("Windows call connects and cleans up without launching Desktop or Voice, and route is part of idempotency", async (t) => {
  const f = fixture(t),
    flow = f.flow({
      ...settings,
      audio: { ...settings.audio, sourceMode: "system_excluding_runtime" },
    });
  const id = randomUUID(),
    call = await flow.request("main", id, "personal", "windows");
  await until(
    () => !!f.journal.callCommand(call.callId, "call.windows.connect")?.receipt,
  );
  assert.ok(!f.methods().some((method) => method.startsWith("call.desktop.")));
  assert.equal(
    (await flow.request("main", id, "personal", "windows")).callId,
    call.callId,
  );
  await assert.rejects(flow.request("main", id, "personal", "voice"));
  await flow.hangup("main", call.callId);
  assert.equal(f.journal.currentCall(), null);
  assert.equal(
    f.methods().filter((method) => method === "call.dial").length,
    1,
  );
});

test("Challenge admission is scoped to the provider peer while an asserted trusted identity retains its bypass", () => {
  const policy = new PhoneAdmission({
    recipients: [],
    incoming: [
      {
        peerAddress: "127.0.0.1",
        transport: "udp",
        fromUri: "sip:known@fixture",
        expectedAssertedNumber: "+49123456789",
      },
    ],
    challengedIncoming: [{ peerAddress: "127.0.0.1", transport: "udp" }],
  });
  const context = {
    sipCallId: "wire",
    fromUri: "sip:known@fixture",
    peerAddress: "127.0.0.1",
    peerPort: 5060,
    transport: "udp" as const,
    assertedNumbers: ["+49123456789"],
  };
  assert.equal(policy.challengesIncoming(context), false);
  assert.equal(
    policy.challengesIncoming({ ...context, assertedNumbers: [] }),
    true,
  );
  assert.equal(
    policy.challengesIncoming({
      ...context,
      peerAddress: "127.0.0.2",
      assertedNumbers: [],
    }),
    false,
  );
});

test("a timed-out call creation waits for its original native owner before spending hangup", async (t) => {
  const f = fixture(t),
    flow = f.flow();
  let creation: PhoneIntent | null = null;
  t.mock.method(
    f.calls,
    "prepare",
    async (_principalId: string, callId: string) => {
      creation = {
        epoch: f.epoch,
        operationId: randomUUID(),
        callId,
        method: "call.prepare",
        requestHash: "sha256:" + "0".repeat(64),
      };
      f.journal.submit(creation);
      throw new IvyError(
        "phone_request_timeout",
        "The native creation has not returned.",
        "unknown",
      );
    },
  );
  const call = await flow.request("main", randomUUID(), "personal");
  await until(() => f.errors.includes("phone_request_timeout"));
  await tick();
  assert.equal(f.journal.currentCall(call.callId)?.callId, call.callId);
  assert.ok(!f.methods().includes("call.hangup"));
  assert.ok(creation);
  f.activate(call.callId);
  f.journal.finish(creation, {
    version: 1,
    epoch: f.epoch,
    requestId: 1,
    ok: true,
    result: { id: call.callId, state: "prepared" },
    error: null,
  });
  await flow.observe();
  await tick();
  if (f.journal.currentCall(call.callId)) await flow.observe();
  assert.equal(f.journal.currentCall(call.callId), null);
  assert.equal(
    f.methods().filter((method) => method === "call.hangup").length,
    1,
  );
});

test("Phone close drains an admission already observing native status and sends no positive call command", async (t) => {
  const f = fixture(t),
    gate = deferred(),
    flow = f.flow();
  f.intercept(async (r) => {
    if (r.method === "status") await gate.promise;
  });
  const pending = flow.request("main", randomUUID(), "personal");
  const failed = assert.rejects(pending, { code: "service_not_ready" });
  await until(() => f.methods().includes("status"));
  const closed = flow.close();
  assert.equal(flow.close(), closed);
  gate.resolve();
  await failed;
  await closed;
  assert.deepEqual(f.methods(), ["status"]);
  assert.equal(f.journal.currentCall(), null);
});

test("Phone flow refuses insufficient complete-call capacity before creating a native call", async (t) => {
  const f = fixture(t, phoneOutcomeReservation * 3),
    flow = f.flow();
  await assert.rejects(flow.request("main", randomUUID(), "personal"), {
    code: "phone_journal_capacity",
  });
  assert.deepEqual(f.methods(), ["status"]);
  assert.equal(f.journal.currentCall(), null);
  assert.equal(f.journal.status().calls, 0);
});

test("Phone admits an arbitrary authenticated principal while retired native epochs cannot replay effects", async (t) => {
  const f = fixture(t),
    flow = f.flow();
  const operation = randomUUID(),
    call = await f.calls.admitOutgoing(
      "stranger",
      operation,
      "personal",
      "voice",
    );
  f.journal.loseEpoch(f.epoch);
  f.journal.beginEpoch(randomUUID());
  assert.deepEqual(await flow.request("stranger", operation, "personal"), call);
  await tick();
  assert.deepEqual(f.methods(), ["status"]);
  await flow.hangup("another-caller", call.callId);
  assert.equal(f.journal.currentCall(call.callId), null);
});

test("loopback probe is trusted idle inspection and creates no retained audio or command", async (t) => {
  const f = fixture(t),
    flow = f.flow(),
    bridge = new PhoneBridge(flow, f.native);
  const report = (await bridge.invoke(
    "probeLoopback",
    {},
    context("arbitrary-host.service"),
  )) as { state: string; detected: boolean };
  assert.equal(report.state, "completed");
  assert.equal(report.detected, false);
  assert.equal(f.journal.status().operations, 0);
  const call = await flow.request("main", randomUUID(), "personal", "windows");
  await until(
    () => !!f.journal.callCommand(call.callId, "call.windows.connect")?.receipt,
  );
  await assert.rejects(bridge.invoke("probeLoopback", {}, context("admin")), {
    code: "phone_call_busy",
  });
  assert.equal(
    f.methods().filter((method) => method === "audio.probe").length,
    1,
  );
});

test("audio setup inspection is available to every trusted caller, reports missing endpoints and has no effects", async (t) => {
  const f = fixture(t),
    bridge = new PhoneBridge(f.flow(), f.native);
  const result = (await bridge.invoke(
    "audioSetup",
    {},
    context("arbitrary-host.service"),
  )) as {
    routes: { route: string; ready: boolean; endpoints: { state: string }[] }[];
  };
  assert.deepEqual(
    result.routes.map((route) => route.route),
    ["windows"],
  );
  assert.ok(
    result.routes.every(
      (route) =>
        !route.ready &&
        route.endpoints.some((endpoint) => endpoint.state === "missing"),
    ),
  );
  assert.deepEqual(f.methods(), ["inventory"]);
  assert.equal(f.journal.status().operations, 0);
});

test("codec diagnostics preserve missing-library evidence and require an idle service", async (t) => {
  const f = fixture(t),
    flow = f.flow(),
    bridge = new PhoneBridge(flow, f.native);
  const report = (await bridge.invoke(
    "codecTest",
    {},
    context("arbitrary-host.service"),
  )) as { passed: boolean; codecs: { name: string; error: string | null }[] };
  assert.equal(report.passed, false);
  assert.equal(
    report.codecs.find((codec) => codec.name === "EVS")?.error,
    "codec_library_missing",
  );
  assert.equal(f.journal.status().operations, 0);
  const call = await flow.request("main", randomUUID(), "personal", "windows");
  await until(
    () => !!f.journal.callCommand(call.callId, "call.windows.connect")?.receipt,
  );
  await assert.rejects(bridge.invoke("codecTest", {}, context("admin")), {
    code: "phone_call_busy",
  });
  assert.equal(
    f.methods().filter((method) => method === "codec.test").length,
    1,
  );
});

test("status exposes all concurrent calls to every trusted principal", async (t) => {
  const f = fixture(t, 512 * 1024 * 1024, 2),
    bridge = new PhoneBridge(f.flow(), f.native);
  const policy = new PhoneAdmission({
    incoming: [],
    recipients: [
      { id: "other", destination: "sip:other@fixture" },
      { id: "personal", destination: "sip:personal@127.0.0.1" },
    ],
  });
  const first = f.journal.admitCall(
    policy.outgoing(f.epoch, randomUUID(), "other", "other", "windows"),
  );
  const second = f.journal.admitCall(
    policy.outgoing(f.epoch, randomUUID(), "main", "personal", "windows"),
  );
  const status = (await bridge.invoke("status", {}, context())) as {
    call: { callId: string };
    calls: { callId: string }[];
    voiceSelection: unknown;
  };
  assert.equal(status.call.callId, first.callId);
  assert.deepEqual(
    status.calls.map((call) => call.callId),
    [first.callId, second.callId],
  );
  assert.equal(
    status.voiceSelection,
    null,
    "Windows audio calls have no Voice model",
  );
  const admin = (await bridge.invoke("calls", {}, context("admin"))) as {
    calls: { callId: string }[];
  };
  assert.deepEqual(
    admin.calls.map((call) => call.callId),
    [first.callId, second.callId],
  );
  assert.ok(f.methods().every((method) => method === "status"));
});

test("audio inventory is available to every trusted caller and remains read-only without call admission", async (t) => {
  const f = fixture(t),
    bridge = new PhoneBridge(f.flow(), f.native);
  const inventory = (await bridge.invoke(
    "inventory",
    {},
    context("arbitrary-host.service"),
  )) as { codecs: { name: string }[] };
  assert.equal(inventory.codecs[0]?.name, "G722");
  assert.deepEqual(f.methods(), ["inventory"]);
  assert.equal(f.journal.status().operations, 0);
  assert.equal(f.credentialReads(), 0);
});

test("Phone status exposes native registration without dialing or reading credentials", async (t) => {
  const f = fixture(t),
    bridge = new PhoneBridge(f.flow(), f.native);
  for (const registration of [
    null,
    { state: "registering", responseCode: null, remoteRemoved: null },
    { state: "registered", responseCode: 200, remoteRemoved: false },
    { state: "failed", responseCode: 403, remoteRemoved: null },
  ]) {
    f.registration(registration);
    const status = (await bridge.invoke("status", {}, context())) as {
      registration: unknown;
      busy: boolean;
    };
    assert.deepEqual(status.registration, registration);
    assert.equal(status.busy, false);
  }
  assert.ok(f.methods().every((method) => method === "status"));
  assert.equal(f.credentialReads(), 0);
  assert.equal(f.journal.currentCall(), null);
});

test("Phone public tools reject caller-supplied identity and retain operation results for every trusted caller", async (t) => {
  const f = fixture(t),
    bridge = new PhoneBridge(f.flow(), f.native);
  validateShared("RegistrySync", phoneRegistry());
  assert.ok(await bridge.invoke("status", {}, context("stranger")));
  const requestsBeforeInvalidInput = f.requests.length;
  for (const [key, value] of Object.entries({
    principalId: "main",
    destination: "sip:other@127.0.0.1",
    desktop,
    password: "secret",
  }))
    await assert.rejects(
      bridge.invoke(
        "request",
        { operationId: randomUUID(), recipientId: "personal", [key]: value },
        context(),
      ),
    );
  assert.equal(f.requests.length, requestsBeforeInvalidInput);
  const call = (await bridge.invoke(
    "request",
    { operationId: randomUUID(), recipientId: "personal" },
    context(),
  )) as { callId: string };
  await until(() => !!f.journal.callCommand(call.callId, "call.dial")?.receipt);
  await tick();
  const receipt = await bridge.invoke(
    "operation",
    { callId: call.callId, method: "call.dial" },
    context(),
  );
  assert.deepEqual(receipt, f.journal.callCommand(call.callId, "call.dial"));
  const before = f.requests.length;
  assert.deepEqual(
    await bridge.invoke(
      "operation",
      { callId: call.callId, method: "call.dial" },
      context("stranger"),
    ),
    receipt,
  );
  assert.equal(f.requests.length, before);
  await bridge.invoke("hangup", { callId: call.callId }, context("admin"));
  const view = (await bridge.invoke(
    "call",
    { callId: call.callId },
    context(),
  )) as { current: boolean; observation: unknown };
  assert.equal(view.current, false);
  assert.equal(view.observation, null);
});

test("Phone public status exposes calls while current policy still controls new effects after revocation", async (t) => {
  const f = fixture(t),
    originalFlow = f.flow(),
    call = await originalFlow.request("main", randomUUID(), "personal");
  await until(() => !!f.journal.callCommand(call.callId, "call.dial")?.receipt);
  await tick();
  const changedPolicy = new PhoneAdmission({
    recipients: [{ id: "another", destination: "sip:another@127.0.0.1" }],
    incoming: [],
  });
  const changedFlow = new PhoneFlow(
    new PhoneCallCommands(changedPolicy, f.journal, f.native),
    f.native,
    { ...settings, outgoingRoute: "windows" },
    async () => ({ username: null, password: null }),
  );
  const bridge = new PhoneBridge(changedFlow, f.native);
  try {
    const status = (await bridge.invoke("status", {}, context("other"))) as {
      busy: boolean;
      call: unknown;
      incoming: unknown;
      recipients: string[];
    };
    assert.equal(status.busy, true);
    assert.deepEqual(status.call, call);
    assert.equal(status.incoming, null);
    assert.deepEqual(status.recipients, ["another"]);
    await assert.rejects(
      bridge.invoke(
        "request",
        { operationId: randomUUID(), recipientId: "personal" },
        context(),
      ),
      { code: "phone_destination_refused" },
    );
    assert.ok(
      await bridge.invoke(
        "operation",
        { callId: call.callId, method: "call.dial" },
        context(),
      ),
    );
    await bridge.invoke("hangup", { callId: call.callId }, context());
    assert.equal(f.journal.currentCall(), null);
  } finally {
    await changedFlow.close();
  }
});

test("default outgoing Windows route and retained history are visible to every trusted principal", async (t) => {
  const { outgoingRoute: _, ...defaults } = settings;
  const f = fixture(t),
    flow = f.flow(defaults),
    bridge = new PhoneBridge(flow, f.native);
  const call = await flow.request("main", randomUUID(), "personal");
  await until(
    () => !!f.journal.callCommand(call.callId, "call.windows.connect")?.receipt,
  );
  assert.equal(call.route, "windows");
  assert.ok(!f.methods().includes("call.desktop.startVoice"));
  await flow.hangup("main", call.callId);
  const mine = (await bridge.invoke("history", {}, context())) as {
    calls: { callId: string }[];
  };
  assert.deepEqual(
    mine.calls.map((c) => c.callId),
    [call.callId],
  );
  const other = (await bridge.invoke("history", {}, context("other"))) as {
    calls: { callId: string }[];
  };
  assert.deepEqual(
    other.calls.map((c) => c.callId),
    [call.callId],
  );
  const admin = (await bridge.invoke("history", {}, context("admin"))) as {
    calls: { callId: string }[];
  };
  assert.deepEqual(
    admin.calls.map((c) => c.callId),
    [call.callId],
  );
});
