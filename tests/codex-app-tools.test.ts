import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  existsSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  CodexAppTools,
  appThreadObservation,
  latestBundledAppToolsServer,
  resolveAppToolsLaunch,
} from "../packages/sdk/src/codex-app-tools.js";
import { DatabaseSync } from "node:sqlite";
import { preparePhoneVoiceArchive } from "../services/phone-bridge/src/runtime/desktop-voice-setup.js";

const tools = [
  "list_projects",
  "list_threads",
  "navigate_to_codex_page",
  "read_thread",
  "create_thread",
  "transfer_voice_call",
  "send_message_to_thread",
  "set_thread_archived",
].map((name) => ({
  name,
  inputSchema:
    name === "list_projects"
      ? { type: "object", properties: {}, additionalProperties: false }
      : name === "create_thread"
      ? { type: "object", properties: { prompt: { type: "string" }, target: { type: "object" },
          title: { type: "string" }, model: { type: "string" }, thinking: { type: "string" } },
          required: ["prompt", "target", "model", "thinking"], additionalProperties: false }
      : name === "list_threads"
      ? {
          type: "object",
          properties: { limit: { type: "integer", maximum: 50 } },
          required: ["limit"],
          additionalProperties: false,
        }
      : {
          type: "object",
          properties: { threadId: { type: "string" },
            ...(name === "send_message_to_thread" ? { model: { type: "string" }, thinking: { type: "string" } } : {}) },
          required: ["threadId"],
          additionalProperties: true,
        },
}));

test("App Tools selects the newest complete installed bundled release", (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-bundled-app-tools-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const name of ["0.1.9", "0.1.10", "0.2.0"])
    mkdirSync(join(root, name));
  writeFileSync(join(root, "0.1.9", "server.mjs"), "");
  writeFileSync(join(root, "0.1.10", "server.mjs"), "");
  assert.equal(latestBundledAppToolsServer(root), join(root, "0.1.10", "server.mjs"));
  writeFileSync(join(root, "0.2.0", "server.mjs"), "");
  assert.equal(latestBundledAppToolsServer(root), join(root, "0.2.0", "server.mjs"));
  assert.deepEqual(resolveAppToolsLaunch({
    nodeExecutable: join(root, "old-node.exe"),
    serverPath: join(root, "0.1.9", "server.mjs"),
    nodeExecutableHash: "sha256:" + "0".repeat(64),
    serverHash: "sha256:" + "0".repeat(64),
    pipePath: "auto", actorThreadId: randomUUID(),
  }, root), { nodeExecutable: process.execPath, serverPath: join(root, "0.2.0", "server.mjs") });
});

test("App Tools normalizes omitted inactive flags without treating unloaded or uncertain tasks as idle", () => {
  const id = randomUUID();
  const observation = (status: unknown, hostId = "local") => ({
    schemaVersion: 1,
    thread: { id, kind: "codex", hostId, status },
  });
  for (const type of ["idle", "notLoaded"])
    assert.deepEqual(appThreadObservation(observation({ type }), id).status, {
      type,
      activeFlags: [],
    });
  assert.deepEqual(
    appThreadObservation(
      observation({ type: "active", activeFlags: ["waitingOnApproval"] }),
      id,
    ).status,
    { type: "active", activeFlags: ["waitingOnApproval"] },
  );
  for (const status of [
    { type: "active" },
    { type: "unknown" },
    { type: "idle", activeFlags: null },
    { type: "idle", activeFlags: [1] },
  ])
    assert.throws(() => appThreadObservation(observation(status), id), {
      code: "app_tools_identity_changed",
    });
  assert.throws(
    () =>
      appThreadObservation(observation({ type: "notLoaded" }, "remote"), id),
    { code: "app_tools_identity_changed" },
  );
  assert.throws(
    () => appThreadObservation(observation({ type: "idle" }), randomUUID()),
    { code: "app_tools_identity_changed" },
  );
});

test("App Tools uses ordinary MCP, explicit actor metadata and one non-replayed archive submission", async (t) => {
  for (const scenario of [
    "setup",
    "normal",
    "unknown",
    "foreign-actor",
    "schema-drift",
    "incompatible-schema",
    "changed-server",
    "intent-failed",
  ])
    await t.test(scenario, async (caseTest) => {
      const root = mkdtempSync(join(tmpdir(), "ivy-app-tools-")),
        server = join(root, "server.mjs"),
        log = server + ".calls";
      caseTest.after(() => rmSync(root, { recursive: true, force: true }));
      const actor = randomUUID(),
        voiceSource = randomUUID(),
        target = randomUUID(),
        operationId = randomUUID();
      writeFileSync(
        server,
        `
import { createInterface } from 'node:readline';
import { appendFileSync } from 'node:fs';
const scenario = ${JSON.stringify(scenario)}, log = ${JSON.stringify(log)}, target = ${JSON.stringify(target)};
const tools = ${JSON.stringify(tools)}.map(tool => scenario === 'schema-drift' && tool.name === 'list_threads'
  ? {...tool, inputSchema: {...tool.inputSchema, properties: {...tool.inputSchema.properties, cursor: {type:'string'}}}}
  : scenario === 'incompatible-schema' && tool.name === 'set_thread_archived'
    ? {...tool, inputSchema: {type:'object', properties:{threadId:{type:'string'}}, required:['threadId'], additionalProperties:false}}
  : tool);
const send = (id,result) => process.stdout.write(JSON.stringify({jsonrpc:'2.0',id,result})+'\\n');
createInterface({input:process.stdin}).on('line', line => {
 const request = JSON.parse(line); if (request.id === undefined) return;
 if (request.method === 'initialize') return send(request.id,{protocolVersion:request.params.protocolVersion,capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}});
 if (request.method === 'tools/list') return send(request.id,{tools});
 if (request.method !== 'tools/call') throw new Error('Unexpected method');
 appendFileSync(log,JSON.stringify(request.params)+'\\n');
 const {name,arguments:args} = request.params;
 if (name === 'set_thread_archived' && scenario === 'unknown') { process.stdout.end(); setTimeout(()=>process.exit(0),10); return; }
 const value = name === 'read_thread' ? {schemaVersion:1,thread:{id:args.threadId,kind:'codex',hostId:scenario==='foreign-actor'?'remote':'local',status:{type:'idle',activeFlags:[]}}} :
   name === 'list_projects' ? {schemaVersion:2,projects:[{projectId:target,projectKind:'local',hostId:'local',path:${JSON.stringify("C:\\Ivy\\PhoneFixture")}}]} :
   name === 'list_threads' ? {schemaVersion:4,pinnedThreads:[{id:target,kind:'codex',hostId:'local',status:'active'}],threads:[]} :
   name === 'navigate_to_codex_page' ? {navigated:true} :
    name === 'transfer_voice_call' ? {transferred:true} :
   name === 'send_message_to_thread' ? {threadId:args.threadId,sent:true} :
   name === 'create_thread' ? {threadId:target,hostId:'local'} : {threadId:args.threadId,archived:true};
 send(request.id,{content:[{type:'text',text:JSON.stringify(value)}]});
});

`,
      );
      const settings = {
        nodeExecutable: process.execPath,
        serverPath: server,
        pipePath: "\\\\.\\pipe\\ivy-fixture-not-connected",
        actorThreadId: actor,
        internalProjectRoot: "C:\\Ivy\\PhoneFixture",
        ...(scenario === "schema-drift"
          ? { inputSchemasHash: "sha256:" + "0".repeat(64) }
          : {}),
      };
      if (scenario === "changed-server")
        writeFileSync(server, 'throw new Error("must never execute");');
      if (scenario === "setup") {
        const databasePath = join(root, "state.sqlite"),
          db = new DatabaseSync(databasePath);
        try {
          db.exec(
            "CREATE TABLE threads(id TEXT PRIMARY KEY,archived INTEGER,thread_source TEXT,recency_at_ms INTEGER); CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);",
          );
          db.prepare("INSERT INTO threads VALUES(?,0,NULL,0)").run(actor);
          const result = await preparePhoneVoiceArchive({
            databasePath,
            nodeExecutable: process.execPath,
            serverPath: server,
            pipePath: settings.pipePath,
            actorThreadId: actor,
            internalProjectRoot: settings.internalProjectRoot,
          });
          assert.deepEqual(result, {
            schemaVersion: 1,
            voiceArchive: { databasePath, appTools: settings },
            candidateCount: 0,
            archived: false,
          });
          assert.ok(
            readFileSync(log, "utf8")
              .trim()
              .split("\n")
              .every((line) => JSON.parse(line).name === "read_thread"),
          );
          assert.equal(
            db.prepare("SELECT archived FROM threads WHERE id=?").get(actor)?.[
              "archived"
            ],
            0,
          );
        } finally {
          db.close();
        }
        return;
      }
      const client = new CodexAppTools({ ...settings,
        nodeExecutableHash: "sha256:" + "0".repeat(64),
        serverHash: "sha256:" + "0".repeat(64),
      });
      let intents = 0,
        promptOperation: string | null = null,
        createOperation: string | null = null;
      try {
        const result = client.archiveThread(target, operationId, async () => {
          if (scenario === "intent-failed")
            throw new Error("durable intent failed");
          intents++;
        });
        if (scenario === "normal" || scenario === "schema-drift") {
          assert.deepEqual(await result, { threadId: target, archived: true });
          assert.deepEqual(await client.listThreads(), [
            { id: target, hostId: "local", kind: "codex", status: "active" },
          ]);
          await client.navigateToThread(target);
          await client.transferVoiceCall(voiceSource, target);
          createOperation = randomUUID();
          assert.equal(await client.createLocalThread('Prepare phone Voice.',
            { model: 'gpt-6-sol', reasoningEffort: 'high' }, createOperation, async () => { intents++; }), target);
          promptOperation = randomUUID();
          assert.deepEqual(
            await client.sendMessage(
              target,
              "Begin the phone conversation.",
              promptOperation,
              async () => {
                intents++;
              },
              scenario === "normal" ? { model: "gpt-6-sol", reasoningEffort: "high" } : {},
            ),
            { threadId: target, sent: true },
          );
        } else if (scenario === "changed-server")
          await assert.rejects(result);
        else
          await assert.rejects(
            result,
            scenario === "unknown"
              ? { code: "app_tools_archive_unknown", outcome: "unknown" }
              : scenario === "foreign-actor"
                ? { code: "app_tools_identity_changed" }
                : scenario === "incompatible-schema"
                  ? { code: "invalid_arguments" }
                : /durable intent failed/,
          );
        assert.equal(
          intents,
          ["normal", "schema-drift"].includes(scenario)
            ? 3
            : scenario === "unknown"
              ? 1
              : 0,
        );
      } finally {
        await client.close();
      }
      try {
        const calls = existsSync(log)
          ? readFileSync(log, "utf8")
              .trim()
              .split("\n")
              .filter(Boolean)
              .map((line) => JSON.parse(line) as Record<string, unknown>)
          : [];
        const archives = calls.filter(
          (call) => call["name"] === "set_thread_archived",
        );
        assert.equal(
          archives.length,
          ["normal", "schema-drift", "unknown"].includes(scenario) ? 1 : 0,
        );
        if (archives[0]) {
          assert.deepEqual(archives[0]["arguments"], {
            threadId: target,
            hostId: "local",
            archived: true,
          });
          assert.deepEqual(archives[0]["_meta"], {
            thread_id: actor,
            call_id: operationId,
          });
        }
        const prompts = calls.filter(
          (call) => call["name"] === "send_message_to_thread",
        );
        assert.equal(prompts.length, ["normal", "schema-drift"].includes(scenario) ? 1 : 0);
        if (prompts[0]) {
          assert.deepEqual(prompts[0]["arguments"], {
            threadId: target,
            hostId: "local",
            prompt: "Begin the phone conversation.",
            ...(scenario === "normal" ? { model: "gpt-6-sol", thinking: "high" } : {}),
          });
          assert.deepEqual(prompts[0]["_meta"], {
            thread_id: actor,
            call_id: promptOperation,
          });
        }
        const creations = calls.filter(call => call['name'] === 'create_thread');
        assert.equal(creations.length, ['normal', 'schema-drift'].includes(scenario) ? 1 : 0);
        if (creations[0]) {
          assert.deepEqual(creations[0]['arguments'], {
            prompt: 'Prepare phone Voice.', target: { type: 'projectless' }, title: 'PhoneBridge Voice',
            model: 'gpt-6-sol', thinking: 'high',
          });
          assert.deepEqual(creations[0]['_meta'], { thread_id: actor, call_id: createOperation });
        }
        const listings = calls.filter(
          (call) => call["name"] === "list_threads",
        );
        assert.equal(listings.length, ["normal", "schema-drift"].includes(scenario) ? 1 : 0);
        if (listings[0])
          assert.deepEqual(listings[0]["arguments"], { limit: 50 });
        const navigations = calls.filter(
          (call) => call["name"] === "navigate_to_codex_page",
        );
        assert.equal(navigations.length, ["normal", "schema-drift"].includes(scenario) ? 1 : 0);
        if (navigations[0]) {
          assert.deepEqual(navigations[0]["arguments"], { threadId: target });
          assert.equal(
            typeof (navigations[0]["_meta"] as Record<string, unknown>)[
              "call_id"
            ],
            "string",
          );
        }
        const transfers = calls.filter(call => call['name'] === 'transfer_voice_call');
        assert.equal(transfers.length, ['normal', 'schema-drift'].includes(scenario) ? 1 : 0);
        if (transfers[0]) {
          assert.deepEqual(transfers[0]['arguments'], { threadId: target });
          assert.equal((transfers[0]['_meta'] as Record<string, unknown>)['thread_id'], voiceSource);
        }
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    });
});

test("Voice transfer distinguishes a definite startup refusal from an uncertain App reply", async (t) => {
  for (const scenario of ["success", "not-ready", "unknown-error", "malformed-success", "wrong-target", "not-transferred"])
    await t.test(scenario, async (caseTest) => {
      const root = mkdtempSync(join(tmpdir(), "ivy-voice-transfer-")),
        server = join(root, "server.mjs"),
        log = server + ".calls",
        actor = randomUUID(),
        source = randomUUID(),
        target = randomUUID();
      caseTest.after(() => rmSync(root, { recursive: true, force: true }));
      writeFileSync(server, `
import { createInterface } from 'node:readline';
import { appendFileSync } from 'node:fs';
const scenario = ${JSON.stringify(scenario)}, log = ${JSON.stringify(log)};
const tools = ${JSON.stringify(tools)};
const send = (id,result) => process.stdout.write(JSON.stringify({jsonrpc:'2.0',id,result})+'\\n');
createInterface({input:process.stdin}).on('line', line => {
  const request = JSON.parse(line); if (request.id === undefined) return;
  if (request.method === 'initialize') return send(request.id,{protocolVersion:request.params.protocolVersion,capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}});
  if (request.method === 'tools/list') return send(request.id,{tools});
  if (request.method !== 'tools/call') throw new Error('Unexpected method');
  appendFileSync(log,JSON.stringify(request.params)+'\\n');
  const {name,arguments:args} = request.params;
  if (name === 'read_thread') return send(request.id,{content:[{type:'text',text:JSON.stringify({schemaVersion:1,thread:{id:args.threadId,kind:'codex',hostId:'local',status:{type:'idle',activeFlags:[]}}})}]});
  if (name !== 'transfer_voice_call') throw new Error('Unexpected tool');
  if (scenario === 'not-ready') return send(request.id,{isError:true,content:[{type:'text',text:'Error: The current voice call could not be transferred to that task.\\n'}]});
  if (scenario === 'unknown-error') return send(request.id,{isError:true,content:[{type:'text',text:'Desktop unavailable after transfer request.'}]});
  send(request.id,{content:[{type:'text',text:scenario === 'malformed-success' ? 'not JSON' : JSON.stringify({transferred:scenario !== 'not-transferred',...(scenario === 'wrong-target' ? {threadId:${JSON.stringify(source)}} : {})})}]});
});
`);
      const client = new CodexAppTools({
        nodeExecutable: process.execPath,
        serverPath: server,
        pipePath: "\\\\.\\pipe\\ivy-fixture-not-connected",
        actorThreadId: actor,
      });
      try {
        await assert.rejects(client.transferVoiceCall(source, target), { code: "app_tools_actor_unverified" });
        await client.verifyActor();
        if (scenario === "success")
          await client.transferVoiceCall(source, target);
        else
          await assert.rejects(client.transferVoiceCall(source, target),
            scenario === "not-ready"
              ? { code: "app_tools_voice_not_ready", outcome: "not_executed" }
              : { code: "app_tools_transfer_unknown", outcome: "unknown" });
      } finally {
        await client.close();
      }
      const calls = readFileSync(log, "utf8").trim().split("\n")
        .map((line) => JSON.parse(line) as Record<string, unknown>);
      assert.deepEqual(calls.map((call) => call["name"]), ["read_thread", "transfer_voice_call"]);
      assert.equal((calls[1]?.["_meta"] as Record<string, unknown>)["thread_id"], source);
    });
});
