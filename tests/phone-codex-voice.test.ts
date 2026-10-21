import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { IvyError } from '../packages/sdk/src/node.js';
import type { Wire } from '../packages/sdk/src/node.js';
import { canonical, digest } from "../packages/contracts/src/canonical.js";
import { phoneVoiceInstructions, phoneVoiceOpeningCue, phoneVoicePreparationPrompt } from "../instructions/phone-voice.js";
import catalog from "../specs/native/codex-0.159.2/catalog.json" with { type: "json" };
import { PhoneCodexVoice } from "../services/phone-bridge/src/runtime/codex-voice.js";
import type { PhoneCodexVoiceSettings } from "../services/phone-bridge/src/runtime/codex-voice.js";
import { selectSpeakerCatalog } from '../services/phone-bridge/src/runtime/speaker-catalog.js';
import { PhoneJournal } from "../services/phone-bridge/src/runtime/journal.js";
import { PhoneAdmission } from "../services/phone-bridge/src/runtime/admission.js";
import type { startCodexProcess } from "../packages/host-runtime/src/codex-process.js";
import type { NativeNotification, NativeRequest } from "../packages/host-runtime/src/codex-rpc.js";

const selection = {
  model: "gpt-6-sol" as const,
  reasoningEffort: "high" as const,
};
function fixture(t: test.TestContext) {
  const nativeCatalog = structuredClone(catalog);
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
  let input!: (value: NativeRequest) => void;
  const answers: unknown[] = [];
  let interruptUnknown = false;
  const threads = new Map<string, Record<string, unknown>>();
  const unloaded = new Set<string>();
  const archived = new Set<string>();
  const archiveRequests: { threadId: string; operationId: string | undefined; detached: boolean | undefined }[] = [];
  const deadLoops = new Set<string>();
  let unloadPause: Promise<void> | undefined;
  let preparationPause: Promise<void> | undefined;
  let losePreparationReply = false;
  let losePromptReply = false;
  let archivePause: Promise<void> | undefined;
  let readPause: Promise<void> | undefined;
  let workReadPause: Promise<void> | undefined;
  let workReadTimeout = false;
  let loseArchiveReply = false;
  let modelTimeout = false;
  let preparationItems: Record<string, Wire.Json>[] = [
    ...['ivy_instructions', 'phone_bridge_status'].map(tool => ({ type: 'mcpToolCall', id: randomUUID(),
      server: 'ivy', tool, status: 'completed', error: null, result: { content: [] } })),
    { type: 'agentMessage', id: randomUUID(), phase: 'final_answer', text: 'READY' },
  ];
  let speakerResponse: unknown = { voices: { v1: ['cove', 'ember'], defaultV1: 'cove', v2: ['marin'], defaultV2: 'marin' } };
  let configuredSpeaker: string | null = null;
  const descendants = (ancestor: string, thread: Record<string, unknown>): boolean => {
    let parent = thread['parentThreadId'];
    while (typeof parent === 'string') {
      if (parent === ancestor) return true;
      parent = threads.get(parent)?.['parentThreadId'];
    }
    return false;
  };
  const model = (name: string, efforts: string[]) => ({ id: name, model: name, displayName: name, description: 'Native catalog fixture',
    hidden: false, isDefault: false, defaultReasoningEffort: efforts[0]!,
    supportedReasoningEfforts: efforts.map(reasoningEffort => ({ reasoningEffort, description: reasoningEffort })) });
  let modelPage = (_params: Record<string, unknown>): unknown => ({
    data: [model('gpt-6.1-sol', ['low', 'high', 'ultra']), model('provider-custom', ['none', 'minimal', 'future-effort'])], nextCursor: null,
  });
  const connect: typeof startCodexProcess = async (options) => {
    notification = options.onNotification;
    input = options.onRequest;
    online = true;
    opens++;
    const rpc = {
      catalog: nativeCatalog,
      get connected() {
        return online;
      },
      async request(
        method: string,
        params: Record<string, unknown>,
        hooks: {
          requestId?: string;
          detachOnTimeout?: boolean;
          beforeResolve?: (id: string, value: unknown) => void;
        } = {},
      ) {
        requests.push({ method, params });
        if (method === 'model/list' && modelTimeout) {
          if (!hooks.detachOnTimeout) online = false;
          throw new IvyError(hooks.detachOnTimeout ? 'native_observation_deadline' : 'native_deadline_exceeded', 'Synthetic slow model discovery.');
        }
        let result: unknown = {};
        if (method === 'thread/realtime/listVoices') result = speakerResponse;
        if (method === 'config/read') result = { config: { realtime: { voice: configuredSpeaker } } };
        if (method === 'model/list') result = modelPage(params);
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
          if (method === 'thread/read' && params['includeTurns'] !== true) await readPause;
          if (!threads.has(id))
            return {
              error: {
                code: -32600,
                message: `no rollout found for thread id ${id}`,
              },
            };
          if (method === "thread/resume") unloaded.delete(id);
          result = { thread: { ...threads.get(id), ...(unloaded.has(id) ? { status: { type: 'notLoaded' } } : {}) } };
          if (method === 'thread/read' && params['includeTurns'] === true) {
            result = structuredClone(result);
            await workReadPause;
            if (workReadTimeout) throw new IvyError('native_observation_deadline', 'Synthetic work observation timeout.', 'unknown',
              { method, timeoutMs: 5000 });
          }
        }
        if (method === 'thread/list') {
          result = { data: [...threads.values()].filter(thread =>
            archived.has(String(thread['id'])) === (params['archived'] === true) &&
            (!params['ancestorThreadId'] || descendants(String(params['ancestorThreadId']), thread)))
            .map(thread => ({ ...thread, ...(unloaded.has(String(thread['id'])) ? { status: { type: 'notLoaded' } } : {}) })), nextCursor: null };
        }
        if (method === 'thread/archive') {
          const id = String(params['threadId']);
          archiveRequests.push({ threadId: id, operationId: hooks.requestId, detached: hooks.detachOnTimeout });
          await archivePause;
          archived.add(id);
          for (const thread of threads.values()) if (descendants(id, thread)) archived.add(String(thread['id']));
          if (loseArchiveReply) { loseArchiveReply = false; throw new Error('Archive acknowledgement lost'); }
        }
        if (method === "thread/unsubscribe") {
          await unloadPause;
          unloaded.add(String(params["threadId"]));
        }
        if (method === "turn/start") {
          const id = String(params["threadId"]);
          const turn = { id: randomUUID(), status: "completed", items: [
            { type: "userMessage", id: randomUUID(), clientId: String(params["clientUserMessageId"]) },
            ...structuredClone(preparationItems),
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
        if (method === "thread/realtime/appendText" && losePromptReply) {
          losePromptReply = false;
          throw new Error("Prompt acknowledgement lost");
        }
        if (method === 'turn/interrupt') {
          if (interruptUnknown) throw new Error('Synthetic lost interrupt outcome');
          const thread = threads.get(String(params['threadId']))!;
          const turn = (thread['turns'] as Record<string, unknown>[]).find(turn => turn['id'] === params['turnId']);
          if (turn) { turn['status'] = 'interrupted'; notification({ method: 'turn/completed', params: { threadId: String(params['threadId']), turn } as Wire.Json }); }
        }
        const reply = { result };
        hooks.beforeResolve?.(hooks.requestId ?? randomUUID(), reply);
        return reply;
      },
      answer(method: string, id: unknown, reply: unknown, beforeSend: () => void) { beforeSend(); answers.push({ method, id, reply }); },
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
  const voice = (runtime: Partial<ConstructorParameters<typeof PhoneCodexVoice>[2]> = {}) => {
    const value = new PhoneCodexVoice(
      settings,
      journal,
      { artifactRoot: root, dataRoot: root, ...runtime },
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
    input: (request: NativeRequest) => input(request),
    answers,
    interruptUnknown: () => { interruptUnknown = true; },
    started: (threadId: string, turnId = randomUUID()) => {
      const turn = { id: turnId, status: 'inProgress', items: [] };
      (threads.get(threadId)!['turns'] as unknown[]).push(turn);
      notification({ method: 'turn/started', params: { threadId, turn } });
      return turnId;
    },
    completed: (threadId: string, turnId: string) => {
      const turn = (threads.get(threadId)!['turns'] as Record<string, unknown>[]).find(turn => turn['id'] === turnId)!;
      turn['status'] = 'completed'; notification({ method: 'turn/completed', params: { threadId, turn } as Wire.Json });
    },
    journal,
    requests,
    threads,
    archived,
    archiveRequests,
    fingerprint: digest(canonical({ home: settings.codexHome, cwd: settings.cwd, projectId: settings.projectId, config: {} })),
    model,
    modelPage: (read: typeof modelPage) => { modelPage = read; },
    modelTimeout: (value: boolean) => { modelTimeout = value; },
    admit,
    release: (call: ReturnType<typeof admit>) => {
      const intent = { epoch: call.epoch, callId: call.callId, operationId: randomUUID(),
        method: "call.release" as const, requestHash: "sha256:" + "0".repeat(64) };
      journal.submit(intent);
      journal.finish(intent, { version: 1, epoch: call.epoch, requestId: 1, ok: true, error: null,
        result: { callId: call.callId, released: true } });
      journal.releaseCall(call.callId, intent.operationId);
    },
    incoming: (caller = "recipient") => journal.admitCall(policy.incoming(epoch, randomUUID(), "incoming-user", {
      id: randomUUID(), direction: "incoming", state: "ringing", error: null, sipCallId: "fixture-wire",
      incoming: { sipCallId: "fixture-wire", peerAddress: "127.0.0.1", peerPort: 5060, transport: "udp", fromUri: `sip:${caller}@127.0.0.1` },
    })),
    settings,
    preparationItems: (items: typeof preparationItems) => { preparationItems = items; },
    speakerResponse: (value: unknown) => { speakerResponse = value; },
    startContract: (versions: string[], voices: string[]) => {
      const schema = nativeCatalog.clientRequests.find(item => item.method === 'thread/realtime/start')!.inputSchema as
        { $defs: Record<string, { enum?: string[] }> };
      Object.values(schema.$defs).find(item => item.enum?.includes('v3'))!.enum = versions;
      Object.values(schema.$defs).find(item => item.enum?.includes('cove'))!.enum = voices;
    },
    unconstrainedStartContract: () => {
      nativeCatalog.clientRequests.find(item => item.method === 'thread/realtime/start')!.inputSchema =
        { $schema: 'https://json-schema.org/draft/2020-12/schema', type: 'object' };
    },
    configuredSpeaker: (value: string | null) => { configuredSpeaker = value; },
    opens: () => opens,
    forgetTasks: () => threads.clear(),
    killLoops: () => { for (const id of threads.keys()) deadLoops.add(id); },
    reviveLoops: () => deadLoops.clear(),
    pauseUnload: (pause: Promise<void>) => {
      unloadPause = pause;
    },
    pausePreparation: (pause: Promise<void>) => { preparationPause = pause; },
    losePreparationReply: () => { losePreparationReply = true; },
    losePromptReply: () => { losePromptReply = true; },
    loseArchiveReply: () => { loseArchiveReply = true; },
    pauseArchive: (pause?: Promise<void>) => { archivePause = pause; },
    pauseRead: (pause?: Promise<void>) => { readPause = pause; },
    pauseWorkRead: (pause?: Promise<void>) => { workReadPause = pause; },
    workReadTimeout: (value: boolean) => { workReadTimeout = value; },
    disconnect: () => {
      online = false;
    },
    notification: (value: NativeNotification) => notification(value),
  };
}

test('Preparation accepts read-only Ivy checks and READY, then passes the conversation assignment to Voice', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit();
  const threadId = await voice.prepare(call, 0, selection);
  const prepare = f.requests.find(item => item.method === 'turn/start')!;
  assert.deepEqual(prepare.params['input'], [{ type: 'text', text: phoneVoicePreparationPrompt, text_elements: [] }]);
  assert.match(phoneVoicePreparationPrompt, /only read-only tools/);
  assert.match(phoneVoicePreparationPrompt, /Do not call, message, change tickets\/settings\/voices, or restart tasks/);
  assert.match(phoneVoicePreparationPrompt, /limits end when the actual call context arrives/);
  assert.match(phoneVoicePreparationPrompt, /regular Ivy MCP catalog/);
  const prompt = 'The caller now asks to change conversation settings.';
  await voice.start(call, 0, threadId, prompt, 'offer', () => {});
  const start = f.requests.find(item => item.method === 'thread/realtime/start')!;
  assert.deepEqual(start.params['initialItems'], [{ role: 'user', text: prompt }]);
  assert.equal(start.params['voice'], 'cove');
  assert.equal(voice.speakerVoice(call.callId), 'cove');
});

test('Preparation rejects NOT_READY, bare READY, ivy_dev-only tools, tool failures and false final acknowledgements', async t => {
  const checks = ['ivy_instructions', 'phone_bridge_status'].map(tool => ({ type: 'mcpToolCall', id: randomUUID(),
    server: 'ivy', tool, status: 'completed', error: null, result: { content: [] } }));
  const ready = { type: 'agentMessage', id: randomUUID(), phase: 'final_answer', text: 'READY' };
  for (const items of [
    [ready],
    [...checks, { ...ready, text: 'NOT_READY: regular catalog unavailable' }],
    [...checks.map(item => ({ ...item, server: 'ivy_dev' })), ready],
    [{ ...checks[0], status: 'failed', error: { message: 'connection lost' } }, checks[1]!, ready],
    [{ ...checks[0], result: { isError: true } }, checks[1]!, ready],
    [checks[0]!, ready],
    [...checks, { ...ready, phase: 'commentary' }],
  ]) {
    const f = fixture(t), voice = f.voice();
    f.preparationItems(items);
    await assert.rejects(voice.prewarm('principal', selection), { code: 'phone_voice_not_ready' });
    // A completed failed readiness check is retained, never silently rerun or accepted on reconnect.
    f.disconnect();
    await assert.rejects(voice.prewarm('principal', selection), { code: 'phone_voice_not_ready' });
    assert.equal(f.requests.filter(item => item.method === 'turn/start').length, 1);
    assert.equal(f.requests.some(item => item.method === 'thread/realtime/start'), false);
  }
});

test('Native speaker discovery uses V3-compatible voices, preserves configured defaults and rejects malformed catalogs', async t => {
  const f = fixture(t), voice = f.voice();
  const first = await voice.speakerVoices();
  assert.deepEqual(first, { protocol: 'v3', catalogVersion: 'v1', defaultVoiceId: 'cove',
    voices: [{ id: 'cove', displayName: 'Cove' }, { id: 'ember', displayName: 'Ember' }] });
  first.voices.length = 0;
  assert.equal((await voice.speakerVoices()).voices.length, 2);
  assert.equal(f.requests.filter(item => item.method === 'thread/realtime/listVoices').length, 1);
  f.configuredSpeaker('ember'); f.disconnect();
  assert.equal((await voice.speakerVoices()).defaultVoiceId, 'ember');
  f.settings.voice = 'cove'; f.disconnect();
  assert.equal((await voice.speakerVoices()).defaultVoiceId, 'cove');
  f.speakerResponse({ voices: { v1: [], defaultV1: 'cove', v2: ['marin'], defaultV2: 'marin' } });
  f.disconnect();
  await assert.rejects(voice.speakerVoices(), { code: 'phone_speaker_catalog_invalid' });
});

test('Speaker catalogs use numeric ordering and only native-compatible start combinations', () => {
  const catalog = { v9: ['nine'], defaultV9: 'nine', v3: ['cove'], defaultV1: 'cove',
    v10: ['ten'], defaultV10: 'ten', v4: ['four'], defaultV4: 'four', v01: ['not-a-version'] };
  for (const entries of [Object.entries(catalog), Object.entries(catalog).reverse()]) {
    const result = selectSpeakerCatalog(Object.fromEntries(entries), protocol => ['v3', 'v4', 'v9', 'v10'].includes(protocol));
    assert.deepEqual(result, { protocol: 'v10', catalogVersion: 'v10', defaultVoiceId: 'ten',
      voices: [{ id: 'ten', displayName: 'Ten' }] });
  }
  const legacy = { v1: ['cove'], defaultV1: 'cove', v2: ['marin'], defaultV2: 'marin' };
  assert.deepEqual(selectSpeakerCatalog(legacy, protocol => protocol === 'v3'),
    { protocol: 'v3', catalogVersion: 'v1', defaultVoiceId: 'cove', voices: [{ id: 'cove', displayName: 'Cove' }] });
  assert.throws(() => selectSpeakerCatalog(legacy, () => false), { code: 'phone_speaker_catalog_invalid' });
  assert.throws(() => selectSpeakerCatalog({ v2: ['marin'], defaultV2: 'marin' }, () => true),
    { code: 'phone_speaker_catalog_invalid' });
  assert.equal(selectSpeakerCatalog(catalog, protocol => protocol === 'v3').catalogVersion, 'v3');
  assert.equal(selectSpeakerCatalog(catalog, (protocol, id) => protocol !== 'v10' || id !== 'ten').catalogVersion, 'v9');
  assert.equal(selectSpeakerCatalog({ ...catalog, v10: [], v9: [] }, () => true).catalogVersion, 'v4');
});

test('Speaker discovery rejects malformed catalogs and defaults outside the chosen group', () => {
  for (const voices of [{}, { v3: [] }, { v0: ['cove'], defaultV0: 'cove' },
    { v3: 'cove' }, { v3: [null] }, { v3: [''] }, { v3: ['cove', 'cove'] },
    { v3: ['x'.repeat(257)] }, { v3: ['cove'] }, { v3: ['cove'], defaultV3: 'marin' },
    { v3: ['sonna'], defaultV1: 'cove' }]) {
    assert.throws(() => selectSpeakerCatalog(voices, () => true), { code: 'phone_speaker_catalog_invalid' });
  }
  const voices = { v1: ['cove'], defaultV1: 'cove', v3: ['sonna'], defaultV3: 'sonna' };
  assert.throws(() => selectSpeakerCatalog(voices, () => true, 'cove'), { code: 'phone_speaker_voice_invalid' });
  assert.throws(() => selectSpeakerCatalog(voices, () => true, ''), { code: 'phone_speaker_voice_invalid' });
  assert.equal(selectSpeakerCatalog(voices, () => true, 'sonna').defaultVoiceId, 'sonna');
  assert.equal(selectSpeakerCatalog({ v3: ['cove', 'sonna'], defaultV1: 'cove' }, () => true).defaultVoiceId, 'cove');
  // A valid explicit override need not use the fallback default of an older group.
  assert.equal(selectSpeakerCatalog({ v3: ['sonna'], defaultV1: 'cove' }, () => true, 'sonna').defaultVoiceId, 'sonna');
});

test('Native discovery and startup use the same selected future catalog and checked protocol', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit();
  const data = { voices: { v1: ['cove'], defaultV1: 'cove', v2: ['marin'], defaultV2: 'marin',
    v3: ['cove', 'sonna'], v4: ['cove', 'miko'], defaultV4: 'miko' } };
  f.speakerResponse(data);
  // Offering v4 alone cannot change the protocol supported by the original native contract.
  assert.equal((await voice.speakerVoices()).catalogVersion, 'v1');
  f.startContract(['v3'], ['cove', 'sonna', 'miko']); f.disconnect();
  assert.equal((await voice.speakerVoices()).catalogVersion, 'v3');
  f.startContract(['v3', 'v4'], ['cove', 'sonna', 'miko']); f.disconnect();
  assert.deepEqual(await voice.speakerVoices(), { protocol: 'v4', catalogVersion: 'v4', defaultVoiceId: 'miko',
    voices: [{ id: 'cove', displayName: 'Cove' }, { id: 'miko', displayName: 'Miko' }] });
  const threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Initial conversation', 'offer', () => {});
  const start = f.requests.find(item => item.method === 'thread/realtime/start')!;
  assert.equal(start.params['version'], 'v4');
  assert.equal(start.params['voice'], 'miko');
  assert.equal(voice.speakerVoice(call.callId), 'miko');
  assert.equal(f.requests.filter(item => item.method === 'thread/realtime/start').length, 1);
});

test('A retained voice from another catalog is rejected before any new native session starts', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit();
  f.startContract(['v3'], ['cove', 'ember', 'sonna']);
  f.speakerResponse({ voices: { v1: ['cove', 'ember'], defaultV1: 'cove', v2: ['marin'], defaultV2: 'marin',
    v3: ['sonna'], defaultV3: 'sonna' } });
  const threadId = await voice.prepare(call, 0, selection);
  const intent = { epoch: call.epoch, callId: call.callId, operationId: randomUUID(), method: 'call.selectSpeakerVoice' as const,
    voiceId: 'ember', requestHash: digest(canonical({ voiceId: 'ember' })) };
  f.journal.submit(intent); f.journal.finishSpeakerVoiceSelection(intent, { selectedVoiceId: 'ember' });
  await assert.rejects(voice.start(call, 0, threadId, 'Initial conversation', 'offer', () => {}),
    { code: 'phone_speaker_voice_invalid' });
  assert.equal(f.requests.some(item => item.method === 'thread/realtime/start'), false);
});

test('An unconstrained native start schema cannot authorize an advertised protocol', async t => {
  const f = fixture(t), voice = f.voice();
  f.unconstrainedStartContract();
  await assert.rejects(voice.speakerVoices(), { code: 'phone_speaker_catalog_invalid' });
  assert.equal(f.requests.some(item => item.method === 'thread/realtime/start'), false);
});

test('Refreshing the catalog does not replace or restart an active Voice session', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit();
  let now = Date.now(); t.mock.method(Date, 'now', () => now);
  const threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Initial conversation', 'offer', () => {});
  f.startContract(['v3', 'v4'], ['cove', 'miko']);
  f.speakerResponse({ voices: { v1: ['cove'], defaultV1: 'cove', v2: ['marin'], defaultV2: 'marin',
    v4: ['miko'], defaultV4: 'miko' } });
  now += 60001;
  assert.equal((await voice.speakerVoices()).catalogVersion, 'v4');
  assert.equal(voice.speakerVoice(call.callId), 'cove');
  assert.equal(f.requests.filter(item => item.method === 'thread/realtime/start').length, 1);
  assert.equal(f.requests.some(item => item.method === 'thread/realtime/stop'), false);
});

test('A retained speaker selection reaches native V3 only after the original call starts its next Voice session', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit();
  const first = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, first, 'Initial conversation', 'offer', () => {});
  const intent = { epoch: call.epoch, callId: call.callId, operationId: randomUUID(), method: 'call.selectSpeakerVoice' as const,
    voiceId: 'ember', requestHash: digest(canonical({ voiceId: 'ember' })) };
  f.journal.submit(intent);
  f.journal.finishSpeakerVoiceSelection(intent, { selectedVoiceId: 'ember' });
  assert.equal(voice.speakerVoice(call.callId), 'cove');
  assert.equal(f.requests.filter(item => item.method === 'thread/realtime/start').length, 1);
  const next = await voice.prepare(call, 1, selection);
  await voice.stop(call.callId, next);
  await voice.start(call, 1, next, 'Restarted conversation', 'offer', () => {});
  assert.equal(f.requests.filter(item => item.method === 'thread/realtime/start').at(-1)!.params['voice'], 'ember');
  assert.equal(voice.speakerVoice(call.callId), 'ember');
});

test('Native model discovery follows pages and coalesces concurrent reads without preparing a task', async t => {
  const f = fixture(t), voice = f.voice();
  f.modelPage(params => params['cursor']
    ? { data: [f.model('provider-custom', ['none', 'minimal', 'future-effort'])], nextCursor: null }
    : { data: [f.model('gpt-6.1-sol', ['low', 'high', 'ultra'])], nextCursor: 'second' });
  const [first, concurrent] = await Promise.all([voice.models(), voice.models()]);
  assert.deepEqual(first, concurrent);
  assert.deepEqual(first.map(item => item.model), ['gpt-6.1-sol', 'provider-custom']);
  assert.deepEqual(first[1]!.supportedReasoningEfforts.map(item => item.reasoningEffort), ['none', 'minimal', 'future-effort']);
  first[0]!.model = 'mutated-client-copy';
  assert.equal((await voice.models())[0]!.model, 'gpt-6.1-sol');
  assert.deepEqual(f.requests.filter(item => item.method === 'model/list').map(item => item.params),
    [{ limit: 100, includeHidden: false }, { limit: 100, includeHidden: false, cursor: 'second' }]);
  assert.equal(f.requests.filter(item => /^(thread|turn)\//.test(item.method)).length, 0);
});

test('Model discovery refreshes after cache expiry and reconnect, and never substitutes a fixed list on failure', async t => {
  const f = fixture(t), voice = f.voice();
  let now = Date.now();
  t.mock.method(Date, 'now', () => now);
  await voice.models();
  f.modelPage(() => ({ data: [f.model('new-native-model', ['new-native-effort'])], nextCursor: null }));
  assert.equal((await voice.models())[0]!.model, 'gpt-6.1-sol');
  now += 60001;
  assert.equal((await voice.models())[0]!.model, 'new-native-model');
  f.modelPage(() => { throw new Error('native catalog unavailable'); });
  f.disconnect();
  await assert.rejects(voice.models(), /native catalog unavailable/);
  f.modelPage(() => ({ data: [f.model('after-reconnect', ['minimal'])], nextCursor: null }));
  assert.equal((await voice.models())[0]!.model, 'after-reconnect');
});

test('a slow model catalogue read preserves the active Voice connection and task', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit();
  const original = await voice.prepare(call, 0, selection);
  f.modelTimeout(true);
  await assert.rejects(voice.models(), { code: 'native_observation_deadline' });
  assert.equal(await voice.prepare(call, 0, selection), original);
  assert.equal(f.opens(), 1);
  f.modelTimeout(false);
  assert.equal((await voice.models()).length, 2);
  assert.equal(f.opens(), 1);
});

test('Model discovery rejects malformed native data and repeated pagination cursors', async t => {
  const f = fixture(t), voice = f.voice();
  f.modelPage(() => ({ data: [{ model: 'incomplete' }], nextCursor: null }));
  await assert.rejects(voice.models());
  f.modelPage(params => ({ data: [f.model(params['cursor'] ? 'second' : 'first', ['high'])], nextCursor: 'repeat' }));
  await assert.rejects(voice.models(), { code: 'phone_voice_catalog_invalid' });
});

test('GPT-6.1 Sol selection reaches native settings and is retained for a fresh replacement task', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit();
  const original = await voice.prepare(call, 0, selection), operationId = randomUUID();
  const chosen = { model: 'gpt-6.1-sol', reasoningEffort: 'ultra' };
  await voice.select(call, original, { operationId, voiceId: 'ember' }, chosen, async () => {});
  assert.deepEqual(f.requests.filter(item => item.method === 'thread/settings/update').at(-1)!.params,
    { threadId: original, model: chosen.model, effort: chosen.reasoningEffort });
  assert.equal(f.journal.get(operationId)?.receipt?.ok, true);
  assert.equal(f.journal.selectedSpeakerVoice(call.callId), 'ember');
  assert.notEqual(await voice.prepare(call, 1, chosen), original);
  const prepared = f.requests.filter(item => item.method === 'thread/start').at(-1)!.params;
  assert.equal(prepared['model'], chosen.model);
  assert.equal((prepared['config'] as Record<string, unknown>)['model_reasoning_effort'], chosen.reasoningEffort);
});

test("Incoming continuation keeps its exact task and speech context, still greets, and *0 restarts fresh", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), threadId = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, threadId, "Discuss the lake walk.", "offer", () => {});
  await voice.connected(first.callId, async () => {});
  const segment = { type: "transcriptSegment", id: "speech-1", realtimeSessionId: "fixture", role: "user", text: "My codeword is Seerose." };
  f.notification({ method: "thread/realtime/item/completed", params: { threadId, item: segment } });
  f.notification({ method: "thread/realtime/item/completed", params: { threadId, item: segment } });
  await voice.stop(first.callId);
  const before = f.requests.length, incoming = f.incoming();
  assert.equal(await voice.prepare(incoming, 0, selection), threadId);
  assert.deepEqual(f.requests.slice(before), [{ method: 'thread/realtime/stop', params: { threadId } }]);
  await voice.start(incoming, 0, threadId, 'Begruesse den User mit "Servus"', "offer", () => {});
  await voice.connected(incoming.callId, async () => {});
  await voice.connected(incoming.callId, async () => {});
  const initial = f.requests.filter(x => x.method === "thread/realtime/start").at(-1)!.params["initialItems"];
  assert.deepEqual(initial, [{ role: "user", text: "Discuss the lake walk." }, { role: "user", text: "My codeword is Seerose." },
    { role: "user", text: 'Begruesse den User mit "Servus"' }]);
  assert.deepEqual(f.requests.filter(x => x.method === "thread/realtime/appendText").map(x => x.params),
    [{ threadId, role: "user", text: phoneVoiceOpeningCue }, { threadId, role: "user", text: phoneVoiceOpeningCue }]);
  assert.equal(f.requests.filter(x => x.method === "turn/start").length, 1);
  const fresh = await voice.prepare(incoming, 1, selection);
  assert.notEqual(fresh, threadId);
  await voice.stop(incoming.callId, fresh);
  await voice.start(incoming, 1, fresh, 'Begruesse den User mit "Servus"', "offer", () => {});
  assert.deepEqual(f.requests.filter(x => x.method === "thread/realtime/start").at(-1)!.params["initialItems"],
    [{ role: "user", text: 'Begruesse den User mit "Servus"' }]);
});

for (const reset of [undefined, true, false]) test(`Outgoing context reset ${reset ?? "default"} controls the next prepared incoming call`, async t => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  if (reset !== undefined) f.settings.resetAfterOutgoingCall = reset;
  const voice = f.voice(), previous = f.incoming(), previousThread = await voice.prepare(previous, 0, selection);
  await voice.start(previous, 0, previousThread, "Previous incoming context", "offer", () => {});
  await voice.connected(previous.callId, async () => {});
  await voice.stop(previous.callId); f.release(previous);
  const outgoing = f.admit(), outgoingThread = await voice.prepare(outgoing, 0, selection);
  assert.notEqual(outgoingThread, previousThread);
  const prompt = "Kannst du mir bitte Chips verkaufen? Ich bin so hungrig";
  await voice.start(outgoing, 0, outgoingThread, prompt, "offer", () => {});
  await voice.connected(outgoing.callId, async () => {});
  await voice.stop(outgoing.callId); f.release(outgoing);
  await voice.prewarm("incoming-user", selection);
  await voice.archive();
  const before = f.requests.length, incoming = f.incoming(), nextThread = await voice.prepare(incoming, 0, selection);
  assert.deepEqual(f.requests.slice(before), reset === false
    ? [{ method: 'thread/realtime/stop', params: { threadId: nextThread } }] : [],
    "a retained conversation confirms media stop without repeating task preparation");
  assert.notEqual(nextThread, previousThread);
  if (reset === false) assert.equal(nextThread, outgoingThread);
  else assert.notEqual(nextThread, outgoingThread);
  const greeting = 'Begruesse den User mit "Servus"';
  await voice.start(incoming, 0, nextThread, greeting, "offer", () => {});
  const start = f.requests.filter(x => x.method === "thread/realtime/start").at(-1)!.params;
  assert.deepEqual(start["initialItems"], [
    ...(reset === false ? [{ role: "user", text: prompt }] : []), { role: "user", text: greeting },
  ]);
  assert.equal(String(start["realtimeStartInstructions"]).includes("Chips"), reset === false);
  assert.equal(f.journal.voiceTask(outgoing.callId, 0), outgoingThread, "reset retains completed call history");
  assert.ok(f.archived.has(previousThread));
  assert.equal(f.archived.has(outgoingThread), reset !== false);
  assert.ok(!f.archived.has(nextThread));
});

test('Background archival keeps the ready and resumable tasks and skips unrelated or busy delegated work', async t => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), call = f.incoming(), continued = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, continued, 'Retained conversation', 'offer', () => {});
  await voice.connected(call.callId, async () => {});
  await voice.stop(call.callId); f.release(call);
  await voice.prewarm('principal', selection); await voice.archive();
  const ready = f.journal.codexTaskCache('prepared-phone-voice', f.fingerprint)!.threadId!;
  const root = { cwd: f.settings.cwd, projectId: f.settings.projectId, ephemeral: false, status: { type: 'idle' }, turns: [] };
  const unrelated = randomUUID(), busy = randomUUID(), child = randomUUID();
  f.threads.set(unrelated, { ...root, id: unrelated, name: 'Phone Voice' });
  f.threads.set(busy, { ...root, id: busy });
  f.threads.set(child, { ...root, id: child, parentThreadId: busy, status: { type: 'inProgress' } });
  f.journal.rememberCodexTask(f.fingerprint, busy);
  await voice.archive();
  assert.deepEqual([...f.archived], []);
  assert.ok(f.requests.some(x => x.method === 'thread/list' && x.params['ancestorThreadId'] === busy &&
    (x.params['sourceKinds'] as string[]).includes('subAgent')));
  f.threads.get(child)!['status'] = { type: 'idle' };
  f.threads.get(busy)!['status'] = { type: 'inProgress' };
  await voice.archive();
  assert.deepEqual([...f.archived], []);
  f.threads.get(busy)!['status'] = { type: 'idle' };
  await voice.archive();
  assert.deepEqual(new Set(f.archived), new Set([busy, child]));
  assert.ok(!f.archived.has(continued) && !f.archived.has(ready) && !f.archived.has(unrelated));
  assert.equal(f.journal.voiceTask(call.callId, 0), continued);
});

test('A delayed background archive never holds up prewarming or an immediate new call', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit(), old = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, old, 'Completed call', 'offer', () => {});
  await voice.stop(call.callId); f.release(call);
  let release!: () => void;
  f.pauseArchive(new Promise<void>(resolve => { release = resolve; }));
  try {
    await voice.prewarm('principal', selection);
    for (let i = 0; i < 20 && !f.archiveRequests.length; i++) await new Promise<void>(resolve => setImmediate(resolve));
    assert.equal(f.archiveRequests.length, 1);
    const original = f.archiveRequests[0]!;
    assert.equal(original.detached, true, 'maintenance deadlines must preserve the call transport');
    assert.equal(f.journal.codexArchivePage(f.fingerprint).find(x => x.threadId === old)!.operationId, original.operationId);
    const before = f.requests.length, next = f.admit(), ready = await voice.prepare(next, 0, selection);
    assert.notEqual(ready, old);
    assert.deepEqual(f.requests.slice(before), [], 'an immediate call uses the already ready task');
    await voice.start(next, 0, ready, 'Hello', 'offer', () => {});
    await voice.connected(next.callId, async () => {});
    assert.ok(!f.archived.has(ready));
  } finally { release(); await voice.archive(); }
  assert.ok(f.archived.has(old));
});

test('An admission during maintenance reads defers archival without blocking the prepared task', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit(), old = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, old, 'Completed call', 'offer', () => {});
  await voice.stop(call.callId); f.release(call);
  let release!: () => void;
  f.pauseRead(new Promise<void>(resolve => { release = resolve; }));
  await voice.prewarm('principal', selection);
  const next = f.admit();
  assert.notEqual(await voice.prepare(next, 0, selection), old);
  release(); await voice.archive();
  assert.deepEqual(f.archiveRequests, []);
});

test('Lost archive acknowledgement reconciles its original request after restart without replay', async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit(), old = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, old, 'Completed call', 'offer', () => {});
  await voice.stop(call.callId); f.release(call);
  f.loseArchiveReply();
  await voice.prewarm('principal', selection); await voice.archive();
  assert.equal(f.archiveRequests.length, 1);
  const original = f.archiveRequests[0]!.operationId;
  assert.equal(f.journal.codexArchivePage(f.fingerprint).find(x => x.threadId === old)!.operationId, original);
  await voice.close();
  const restarted = f.voice();
  await restarted.prewarm('principal', selection); await restarted.archive();
  assert.equal(f.archiveRequests.length, 1);
  assert.ok(f.requests.some(x => x.method === 'thread/list' && x.params['archived'] === true));
  assert.ok(!f.journal.codexArchivePage(f.fingerprint).some(x => x.threadId === old));
  f.journal.rememberCodexTask(f.fingerprint, old);
  assert.ok(!f.journal.codexArchivePage(f.fingerprint).some(x => x.threadId === old), 'the retained receipt prevents rediscovery from replaying archival');
  assert.equal(f.journal.voiceTask(call.callId, 0), old);
});

test('Historical native bindings recover only tasks proven to belong to this exact configured runtime', async t => {
  const f = fixture(t), voice = f.voice();
  await voice.prewarm('principal', selection); await voice.archive();
  const owned = randomUUID(), other = randomUUID();
  for (const threadId of [owned, other]) {
    f.threads.set(threadId, { id: threadId, cwd: f.settings.cwd, projectId: f.settings.projectId,
      ephemeral: false, status: { type: 'idle' }, turns: [] });
    const call = f.admit(), generation = 0;
    const intent = { epoch: call.epoch, callId: call.callId, method: 'call.bindVoice' as const, operationId: randomUUID(),
      threadId, voiceGeneration: generation, requestHash: digest(canonical({ threadId, generation,
        fingerprint: threadId === owned ? f.fingerprint : digest('other configured home') })) };
    f.journal.submit(intent); f.journal.finishVoiceBinding(intent); f.release(call);
  }
  await voice.archive();
  assert.deepEqual([...f.archived], [owned]);
  assert.deepEqual(f.journal.codexBindingPage().map(x => x.intent.method), ['call.bindVoice', 'call.bindVoice']);
  assert.equal(f.journal.codexBindingPage('', 1).length, 1);
  const cursor = f.journal.codexBindingPage('', 1)[0]!.intent.operationId;
  assert.equal(f.journal.codexBindingPage(cursor).length, 1);
});

test("An outgoing Ivy call never resumes the previous incoming conversation", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), old = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, old, "Previous context", "offer", () => {});
  await voice.connected(first.callId, async () => {});
  await voice.stop(first.callId);
  const outgoing = f.admit(), fresh = await voice.prepare(outgoing, 0, selection);
  assert.notEqual(fresh, old);
  await voice.start(outgoing, 0, fresh, "New assignment", "offer", () => {});
  await voice.connected(outgoing.callId, async () => {});
  assert.deepEqual(f.requests.filter(x => x.method === "thread/realtime/start").at(-1)!.params["initialItems"],
    [{ role: "user", text: "New assignment" }]);
  assert.deepEqual(f.requests.filter(x => x.method === "thread/realtime/appendText").at(-1)!.params,
    { threadId: fresh, role: "user", text: phoneVoiceOpeningCue });
});

test("A different admitted incoming caller cannot inherit the previous conversation", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), old = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, old, "Private first context", "offer", () => {});
  await voice.connected(first.callId, async () => {});
  await voice.stop(first.callId);
  assert.notEqual(await voice.prepare(f.incoming("other"), 0, selection), old);
});

test("An uncertain stop cannot expose the still-owned task as an incoming continuation", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), old = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, old, "Original context", "offer", () => {});
  await voice.connected(first.callId, async () => {});
  f.killLoops();
  await assert.rejects(voice.stop(first.callId));
  const next = f.incoming();
  assert.notEqual(await voice.prepare(next, 0, selection), old);
  f.reviveLoops();
  await voice.stop(first.callId);
});

test("A continued native task is reconciled after reconnect without another READY preparation turn", async (t) => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const firstVoice = f.voice(), first = f.incoming(), old = await firstVoice.prepare(first, 0, selection);
  await firstVoice.start(first, 0, old, "Retained context", "offer", () => {});
  await firstVoice.connected(first.callId, async () => {});
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

test("An unknown unused creation still warms incoming continuation without repeating that creation", async t => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const first = f.voice(), call = f.incoming(), threadId = await first.prepare(call, 0, selection);
  await first.start(call, 0, threadId, "Retained conversation", "offer", () => {});
  await first.connected(call.callId, async () => {});
  await first.stop(call.callId); f.release(call); await first.close();
  const creationOperationId = randomUUID();
  f.journal.retainCodexTask("prepared-phone-voice", { fingerprint: f.fingerprint, threadId: null, creationOperationId });
  const second = f.voice(), before = f.requests.length;
  await assert.rejects(second.prewarm("incoming-user", selection), { code: "phone_voice_create_unknown" });
  assert.ok(f.requests.slice(before).some(x => x.method === "thread/resume" && x.params["threadId"] === threadId));
  assert.equal(f.requests.filter(x => x.method === "thread/start").length, 1);
  assert.equal(f.journal.codexTaskCache("prepared-phone-voice", f.fingerprint)?.creationOperationId, creationOperationId);
  const warm = f.requests.length;
  assert.equal(await second.prepare(f.incoming(), 0, selection), threadId);
  assert.deepEqual(f.requests.slice(warm), [{ method: 'thread/realtime/stop', params: { threadId } }],
    "the incoming continuation confirms media stop without repeating task preparation");
});

test("Slow unused model preparation does not delay an already warmed incoming continuation", async t => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const first = f.voice(), call = f.incoming(), threadId = await first.prepare(call, 0, selection);
  await first.start(call, 0, threadId, "Retained conversation", "offer", () => {});
  await first.connected(call.callId, async () => {});
  await first.stop(call.callId); f.release(call); await first.close();
  let release!: () => void;
  f.pausePreparation(new Promise<void>(resolve => { release = resolve; }));
  const second = f.voice(), warm = second.prewarm("incoming-user", selection);
  let finished = false; void warm.then(() => { finished = true; });
  try {
    for (let i = 0; i < 100 && f.requests.filter(x => x.method === "turn/start").length < 2; i++)
      await new Promise(resolve => setTimeout(resolve, 1));
    assert.equal(f.requests.filter(x => x.method === "turn/start").length, 2);
    assert.equal(finished, false);
    const before = f.requests.length;
    assert.equal(await second.prepare(f.incoming(), 0, selection), threadId);
    assert.deepEqual(f.requests.slice(before), [{ method: 'thread/realtime/stop', params: { threadId } }],
      "continuation confirms its own media stop without awaiting the other task's preparation");
  } finally { release(); await warm; }
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

test('A cached incoming continuation confirms media stop and replaces a dead loop before new startup', async t => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), threadId = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(first.callId, async () => {});
  await voice.stop(first.callId); f.release(first);
  f.killLoops();
  const next = f.incoming();
  const replacement = await voice.prepare(next, 0, selection);
  assert.notEqual(replacement, threadId);
  assert.equal(f.journal.voiceTask(first.callId, 0), threadId);
  assert.equal(f.requests.filter(request => request.method === 'thread/realtime/start').length, 1,
    'a broken cached media session is rejected before another start');
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
  assert.equal(started["prompt"], phoneVoiceInstructions);
  assert.equal(started["codexResponseHandoffMode"], "commentary");
  assert.ok(
    String(started["realtimeStartInstructions"]).endsWith(
      JSON.stringify(prompt),
    ),
  );
  assert.equal(started["clientManagedHandoffs"], false);
  assert.equal(started["includeStartupContext"], false);
  assert.equal(f.journal.callCommand(call.callId, "call.promptVoice", 0)?.phase, "submitted");
  assert.equal(f.requests.filter(x => x.method === "thread/realtime/appendText").length, 0);
  await Promise.all([second.connected(call.callId, async () => {}), second.connected(call.callId, async () => {})]);
  await second.connected(call.callId, async () => {});
  assert.deepEqual(f.requests.filter(x => x.method === "thread/realtime/appendText").map(x => x.params),
    [{ threadId, role: "user", text: phoneVoiceOpeningCue }]);
  assert.deepEqual(f.journal.callCommand(call.callId, "call.promptVoice", 0)?.receipt?.result, { threadId, state: "sent" });
  await second.stop(call.callId);
  assert.equal(
    f.journal.callCommand(call.callId, "call.stopVoice")?.receipt?.ok,
    true,
  );
});

test("A lost opening acknowledgement retains the full assignment without repeating its opening", async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit(), threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, "Ask the caller about lunch.", "offer", () => {});
  f.losePromptReply();
  await assert.rejects(voice.connected(call.callId, async () => {}), /Prompt acknowledgement lost/);
  await assert.rejects(voice.connected(call.callId, async () => {}), /Prompt acknowledgement lost/);
  assert.equal(f.requests.filter(x => x.method === "thread/realtime/appendText").length, 1);
  assert.deepEqual(f.journal.callCommand(call.callId, "call.promptVoice", 0)?.receipt?.result, { threadId, state: "outcome_unknown" });
});

test("A long new assignment keeps recent continuation context within both native startup bounds", async t => {
  const f = fixture(t); f.settings.resumeIncomingConversation = true;
  const voice = f.voice(), first = f.incoming(), threadId = await voice.prepare(first, 0, selection);
  await voice.start(first, 0, threadId, "Previous assignment", "offer", () => {});
  await voice.connected(first.callId, async () => {});
  for (const [id, text] of [["escaped", '"'.repeat(10500)], ["recent", "The agreed codeword is Seerose."]] as const)
    f.notification({ method: "thread/realtime/item/completed", params: { threadId,
      item: { type: "transcriptSegment", id, role: "user", text } } });
  await voice.stop(first.callId);
  const incoming = f.incoming(), prompt = "New assignment: " + "x".repeat(18000);
  assert.equal(await voice.prepare(incoming, 0, selection), threadId);
  await voice.start(incoming, 0, threadId, prompt, "offer", () => {});
  const start = f.requests.filter(x => x.method === "thread/realtime/start").at(-1)!.params;
  const items = start["initialItems"] as { role: string; text: string }[];
  assert.deepEqual(items.at(-1), { role: "user", text: prompt });
  assert.ok(items.some(x => x.text === "The agreed codeword is Seerose."));
  assert.ok(items.length <= 128);
  assert.ok(items.reduce((tokens, item) => tokens + Math.ceil(Buffer.byteLength(item.text) / 4), 0) <= 8192);
  assert.ok(Buffer.byteLength(String(start["realtimeStartInstructions"])) <= 32768);
  assert.ok(String(start["realtimeStartInstructions"]).includes(JSON.stringify(prompt)));
});

test("A call ending after media setup cannot dispatch its queued initial prompt", async t => {
  const f = fixture(t), voice = f.voice(), call = f.admit(), threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, "Ask the caller about lunch.", "offer", () => {});
  await voice.stop(call.callId);
  await voice.connected(call.callId, async () => {});
  assert.equal(f.requests.filter(x => x.method === "thread/realtime/appendText").length, 0);
  assert.deepEqual(f.journal.callCommand(call.callId, "call.promptVoice", 0)?.receipt?.result, { threadId, state: "outcome_unknown" });
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

test('A slow work snapshot cannot delay Voice stop or overwrite a newer completed turn', async t => {
  const f = fixture(t), voice = f.voice(), call = f.incoming();
  const threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(call.callId, async () => {});
  const turnId = f.started(threadId);
  let release!: () => void;
  f.pauseWorkRead(new Promise<void>(resolve => { release = resolve; }));
  t.after(release);
  await voice.detach(call.callId, Date.now());
  f.release(call);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(f.journal.callCommand(call.callId, 'call.stopVoice')!.receipt!.ok, true);
  assert.equal(f.journal.voiceObservations().length, 1);
  f.completed(threadId, turnId);
  release();
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(voice.work(call.callId)!.state, 'completed');
  assert.deepEqual(f.journal.voiceObservations(), []);
});

test('A timed-out observation recovers without a new call and retains the original work deadline', async t => {
  const f = fixture(t);
  let now = Date.now();
  const observed: (string | null)[] = [];
  const voice = f.voice({ now: () => now, workObserved: (_id, code) => observed.push(code) });
  const call = f.incoming(), threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(call.callId, async () => {});
  f.started(threadId);
  f.workReadTimeout(true);
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const end = now - 20_000;
  await voice.detach(call.callId, end);
  f.release(call);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.deepEqual(observed, ['native_observation_deadline']);
  assert.equal(voice.work(call.callId)!.deadline, end + 300_000);
  assert.equal(f.journal.voiceObservations().length, 1);
  f.workReadTimeout(false);
  now += 15_000;
  t.mock.timers.tick(1000);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.deepEqual(observed, ['native_observation_deadline', null]);
  assert.equal(voice.work(call.callId)!.deadline, end + 300_000);
  assert.deepEqual(f.journal.voiceObservations(), []);
  t.mock.timers.reset();
});

test('An old delayed snapshot cannot detach or change the turn of a rejoined live call', async t => {
  const f = fixture(t), observations: (string | null)[] = [];
  const voice = f.voice({ workObserved: (_id, code) => observations.push(code) });
  const call = f.incoming(), threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(call.callId, async () => {});
  const turnId = f.started(threadId);
  let release!: () => void;
  f.pauseWorkRead(new Promise<void>(resolve => { release = resolve; }));
  t.after(release);
  await voice.detach(call.callId, Date.now());
  f.release(call);
  await new Promise<void>(resolve => setImmediate(resolve));
  f.pauseWorkRead(); // The old read remains paused; new exact-task reads may complete.
  const redial = f.incoming();
  assert.equal(await voice.prepare(redial, 0, selection), threadId);
  await voice.start(redial, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(redial.callId, async () => {});
  release();
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(voice.work(redial.callId)!.turnId, turnId);
  assert.equal(voice.work(redial.callId)!.attachedCallId, redial.callId);
  assert.equal(voice.work(redial.callId)!.deadline, null);
  assert.deepEqual(observations, [null]);
});

test('An unobserved turn survives release/restart and is interrupted against the first end time', async t => {
  const f = fixture(t);
  let now = Date.now();
  const voice = f.voice({ now: () => now });
  const call = f.incoming(), threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(call.callId, async () => {});
  const turnId = randomUUID();
  (f.threads.get(threadId)!['turns'] as unknown[]).push({ id: turnId, status: 'inProgress', items: [] });
  f.workReadTimeout(true);
  const end = now;
  await voice.detach(call.callId, end);
  f.release(call);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(voice.work(call.callId), null, 'the started notification was missed');
  await voice.close();
  f.workReadTimeout(false);
  now += 300_001;
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const recovered = f.voice({ now: () => now });
  t.mock.timers.tick(1000);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(recovered.work(call.callId)!.deadline, end + 300_000);
  assert.equal(recovered.work(call.callId)!.state, 'paused_deadline');
  assert.deepEqual(f.requests.filter(request => request.method === 'turn/interrupt').map(request => request.params), [{ threadId, turnId }]);
  t.mock.timers.reset();
});

test('Hangup retains the backing turn and input; authorized redial attaches without another task or turn', async t => {
  const f = fixture(t);
  f.settings.resumeIncomingConversation = true;
  f.settings.keepTaskLoaded = false;
  let now = Date.now();
  const notices: Record<string, unknown>[] = [];
  const voice = f.voice({ now: () => now, notifyInput: async (_work, input) => { notices.push(input); return true; } });
  const call = f.incoming(), threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(call.callId, async () => {});
  const turnId = f.started(threadId);
  f.input({ id: 17, method: 'item/tool/requestUserInput', params: { threadId, turnId, itemId: 'question',
    questions: [{ id: 'q', header: 'Choice', question: 'Which destination?', isOther: false, isSecret: false, options: null }] } });
  assert.equal(notices.length, 0, 'no post-hangup notification while caller is connected');
  const end = now - 20_000;
  await voice.detach(call.callId, end);
  f.release(call);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(voice.work(call.callId)!.deadline, end + 300_000);
  assert.equal(notices.length, 1);
  assert.equal(f.answers.length, 0, 'notification is not consent');
  await voice.prewarm('incoming-user', selection);
  await voice.archive();
  assert.equal(f.archived.has(threadId), false);
  assert.equal(f.requests.filter(request => request.method === 'thread/unsubscribe').length, 0);
  const baseline = f.requests.length;
  const redial = f.incoming();
  assert.equal(await voice.prepare(redial, 0, selection), threadId);
  await voice.start(redial, 0, threadId, 'Hi', 'new offer', () => {});
  const startup = f.requests.at(-1)!;
  assert.equal(startup.method, 'thread/realtime/start');
  assert.equal('realtimeStartInstructions' in startup.params, false);
  assert.equal(voice.work(redial.callId), null, 'negotiation alone cannot reset the old deadline');
  await voice.connected(redial.callId, async () => {});
  assert.equal(voice.work(redial.callId)!.turnId, turnId);
  assert.equal(voice.work(redial.callId)!.deadline, null);
  assert.equal(f.requests.slice(baseline).some(request => ['thread/start', 'turn/start', 'thread/settings/update'].includes(request.method)), false);
  const pending = voice.inputs(redial.callId)[0]!;
  assert.throws(() => voice.answer(redial.callId, 17, { result: { answers: { q: { answers: ['A'] } } } }, 'stale'), /no longer/);
  voice.answer(redial.callId, 17, { result: { answers: { q: { answers: ['A'] } } } }, String(pending['inputToken']));
  assert.equal(f.answers.length, 1);
  assert.equal(voice.inputs(call.callId).length, 0);
  now += 400_000;
  await voice.detach(redial.callId, now);
  assert.equal(voice.work(redial.callId)!.deadline, now + 300_000, 'second real connection owns a new allowance');
  f.completed(threadId, turnId);
  assert.equal(voice.work(redial.callId)!.state, 'completed');
});

test('Independent deadline interrupts the original waiting turn and rejects a late answer', async t => {
  const f = fixture(t);
  let now = Date.now();
  const voice = f.voice({ now: () => now });
  const call = f.incoming(), threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(call.callId, async () => {});
  const turnId = f.started(threadId);
  f.input({ id: 'approval', method: 'item/commandExecution/requestApproval', params: { threadId, turnId, itemId: 'command', startedAtMs: now } });
  const pending = voice.inputs(call.callId)[0]!;
  await voice.detach(call.callId, now);
  f.release(call);
  t.mock.timers.enable({ apis: ['setTimeout'] });
  // Reschedule once under the test clock without changing the actual stored deadline.
  const other = f.incoming('other');
  await voice.prepare(other, 0, selection);
  await voice.detach(other.callId, now);
  f.release(other);
  now += 300_000;
  t.mock.timers.tick(1000);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.throws(() => voice.answer(call.callId, 'approval', { result: { decision: 'accept' } }, String(pending['inputToken'])), /ended session|no longer/);
  assert.deepEqual(f.requests.filter(request => request.method === 'turn/interrupt').map(request => request.params), [{ threadId, turnId }]);
  assert.equal(voice.work(call.callId)!.state, 'paused_deadline');
  assert.equal(f.answers.length, 0);
  t.mock.timers.tick(5000);
  assert.equal(f.requests.filter(request => request.method === 'turn/interrupt').length, 1);
  t.mock.timers.reset();
});

test('An uncertain interrupt retains the original work and cannot be auto-resumed', async t => {
  const f = fixture(t);
  const voice = f.voice();
  const call = f.incoming(), threadId = await voice.prepare(call, 0, selection);
  await voice.start(call, 0, threadId, 'Hi', 'offer', () => {});
  await voice.connected(call.callId, async () => {});
  f.started(threadId); f.interruptUnknown();
  await voice.detach(call.callId, Date.now() - 300_001);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(voice.work(call.callId)!.state, 'outcome_unknown');
  f.release(call);
  const next = f.incoming();
  assert.notEqual(await voice.prepare(next, 0, selection), threadId);
  assert.equal(f.requests.filter(request => request.method === 'turn/interrupt').length, 1);
});
