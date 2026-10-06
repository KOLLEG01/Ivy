import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { readFileSync } from "node:fs";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { NativeContract } from "../packages/contracts/src/native-contract.js";
import { SchemaValidators } from "../packages/contracts/src/schema.js";
import { NativeRpc } from "../services/agent-manager/src/rpc.js";
import type { NativeRequest } from "../services/agent-manager/src/rpc.js";
import {
  assertClaudeSubscriptionConfig,
  checkedClaudeAdapter,
} from "../services/agent-manager/src/claude-adapter.js";
import type { Agent, Wire } from "../packages/contracts/src/generated.js";

const catalog = JSON.parse(
  readFileSync("specs/native/codex-0.142.3/catalog.json", "utf8"),
) as Agent.Catalog;
const contract = new NativeContract(catalog);

test("Claude adapter catalog exposes only the Ivy lifecycle surface", () => {
  const methods = new Set(catalog.clientRequests.map((value) => value.method));
  for (const method of [
    "initialize",
    "thread/start",
    "thread/resume",
    "thread/list",
    "thread/read",
    "thread/turns/list",
    "thread/items/list",
    "thread/loaded/list",
    "turn/start",
    "turn/interrupt",
  ])
    assert.ok(methods.has(method), method);
  assert.equal(methods.has("account/login/start"), false);
  assert.equal(methods.has("config/value/write"), false);
  assert.equal(methods.has("thread/fork"), false);
});

test(
  "pinned Claude adapter bundle detects changed executable bytes",
  { skip: !process.env.IVY_CLAUDE_ADAPTER_ROOT },
  async () => {
    const root = process.env.IVY_CLAUDE_ADAPTER_ROOT!;
    assert.ok(
      (await checkedClaudeAdapter(root, catalog.nativeExecutableHash)).endsWith(
        "adapter.mjs",
      ),
    );
    await assert.rejects(
      checkedClaudeAdapter(root, "sha256:" + "0".repeat(64)),
      { code: "native_build_mismatch" },
    );
  },
);

test("Claude subscription preflight rejects API billing settings without reading values into diagnostics", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-claude-billing-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await assertClaudeSubscriptionConfig(root, {});
  await writeFile(
    join(root, "settings.json"),
    JSON.stringify({ env: { ANTHROPIC_API_KEY: "secret-value" } }),
  );
  await assert.rejects(assertClaudeSubscriptionConfig(root, {}), {
    code: "billing_configuration_conflict",
  });
  await writeFile(join(root, "settings.json"), "{}");
  await assert.rejects(
    assertClaudeSubscriptionConfig(root, { CLAUDE_CODE_USE_BEDROCK: "1" }),
    { code: "billing_configuration_conflict" },
  );
});

test("pinned Claude runtime selects filesystem skill sources and receives managed instructions and MCP", { skip: !process.env.IVY_CLAUDE_ADAPTER_ROOT }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-claude-environment-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const instructions = join(root, "AGENTS.override.md");
  await writeFile(instructions, "Managed guidance\n\n@AGENTS.md\n");
  await writeFile(join(root, "AGENTS.md"), "Project guidance\n");
  const previous = process.env.CLAUDE_CODEX_INSTRUCTIONS_FILE;
  process.env.CLAUDE_CODEX_INSTRUCTIONS_FILE = instructions;
  t.after(() => { if (previous === undefined) delete process.env.CLAUDE_CODEX_INSTRUCTIONS_FILE;
    else process.env.CLAUDE_CODEX_INSTRUCTIONS_FILE = previous; });
  const module = await import(pathToFileURL(join(process.env.IVY_CLAUDE_ADAPTER_ROOT!, "dist/src/native-runtime.mjs")).href);
  const runtime = new module.NativeClaudeRuntime();
  const mcpServers = { ivy: { type: "http", url: "https://hive.example/mcp" } };
  const context = { cwd: root, threadId: "thread", turnId: "turn", prompt: "Check environment", runtimeType: "agent-sdk-sidecar",
    model: null, effort: null, claudeSessionId: null, forkSession: false, mcpServers, allowedTools: null,
    addDirs: [], enableFileCheckpointing: false, outputFormat: null, approvalPolicy: "on-request",
    sandboxMode: "workspace-write", systemPromptAddendum: null, planMode: false, imageInputs: [] };
  const options = runtime.buildOptions({}, context, new AbortController()) as Record<string, unknown>;
  assert.deepEqual(options.settingSources, ["user", "project", "local"]);
  assert.deepEqual(options.mcpServers, mcpServers);
  const prompt = options.systemPrompt as { type: string; preset: string; append: string };
  assert.equal(prompt.type, "preset");
  assert.equal(prompt.preset, "claude_code");
  assert.match(prompt.append, /Managed guidance\n\nProject guidance/);
  assert.doesNotMatch(prompt.append, /@AGENTS\.md/);
});

test("Claude model discovery preserves runtime capabilities without submitting an inference turn", { skip: !process.env.IVY_CLAUDE_ADAPTER_ROOT }, async (t) => {
  const root = process.env.IVY_CLAUDE_ADAPTER_ROOT!;
  const native = await import(pathToFileURL(join(root, "dist/src/native-runtime.mjs")).href);
  const projection = await import(pathToFileURL(join(root, "dist/src/model-discovery.mjs")).href);
  const runtime = new native.NativeClaudeRuntime();
  const available = [
    { value: "default", resolvedModel: "claude-opus-5-5", displayName: "Default", description: "Current default", supportsEffort: true, supportedEffortLevels: ["low", "high", "max"] },
    { value: "opus", resolvedModel: "claude-opus-5-5", displayName: "Opus", description: "Current Opus", supportsEffort: true, supportedEffortLevels: ["low", "high", "max"] },
    { value: "claude-opus-5", resolvedModel: "claude-opus-5", displayName: "Opus", description: "Another available Opus", supportsEffort: true, supportedEffortLevels: ["high"] },
    { value: "claude-future[1m]", resolvedModel: "claude-future", displayName: "Future", description: "Current future model", supportsEffort: true, supportedEffortLevels: ["provider-effort"] },
    { value: "haiku", resolvedModel: "claude-haiku-4-5-20251001", displayName: "Haiku", description: "Current Haiku" },
  ];
  let request: { prompt: AsyncIterable<unknown>; options: Record<string, unknown> } | undefined;
  let closed = false;
  runtime.sdk = { query: (input: typeof request) => {
    request = input;
    return { supportedModels: async () => available, close: () => { closed = true; } };
  } };
  const discovered = await runtime.supportedModels();
  assert.deepEqual(discovered, available);
  assert.equal(closed, true);
  assert.equal(request!.options.persistSession, false);
  assert.deepEqual(request!.options.tools, []);
  assert.equal((await request!.prompt[Symbol.asyncIterator]().next()).done, true);
  const models = projection.runtimeModelList(discovered, "claude-opus-5-5", "high");
  contract.validateResult("model/list", models);
  assert.deepEqual(models.data.map((model: { model: string }) => model.model), ["claude-opus-5-5", "claude-opus-5", "claude-future[1m]", "claude-haiku-4-5-20251001"]);
  assert.deepEqual(models.data.map((model: { displayName: string }) => model.displayName), ["Opus 5.5", "Opus 5", "Future", "Haiku 4.5"]);
  assert.equal(models.data[0].isDefault, true);
  assert.deepEqual(models.data[0].supportedReasoningEfforts.map((effort: { reasoningEffort: string }) => effort.reasoningEffort), ["low", "high", "max"]);
  assert.deepEqual(models.data[2].supportedReasoningEfforts.map((effort: { reasoningEffort: string }) => effort.reasoningEffort), ["provider-effort"]);
  assert.deepEqual(models.data[3].supportedReasoningEfforts, []);
  assert.throws(() => projection.runtimeModelList([], "missing", "high"), /no available models/);
  const previousDefault = process.env.CLAUDE_CODEX_DEFAULT_MODEL;
  const previousProxy = process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY;
  process.env.CLAUDE_CODEX_DEFAULT_MODEL = "claude-example-opus";
  process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY = "1";
  t.after(() => {
    if (previousDefault === undefined) delete process.env.CLAUDE_CODEX_DEFAULT_MODEL;
    else process.env.CLAUDE_CODEX_DEFAULT_MODEL = previousDefault;
    if (previousProxy === undefined) delete process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY;
    else process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY = previousProxy;
  });
  const helpers = await import(pathToFileURL(join(root, "dist/src/server-helpers.mjs")).href);
  assert.equal(helpers.defaultSelectableModelId(), "claude-example-opus");
  assert.equal(helpers.normalizeSelectableModelId("claude-future[1m]", "claude-example-opus"), "claude-future[1m]");
  assert.equal(helpers.reasoningEffortFromParams({ effort: "max" }, null), "max");
});

test(
  "pinned Claude mock speaks the catalog lifecycle without Codex fallback",
  { skip: !process.env.IVY_CLAUDE_ADAPTER_ROOT },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "ivy-claude-adapter-"));
    const adapter = await checkedClaudeAdapter(
      process.env.IVY_CLAUDE_ADAPTER_ROOT!,
      catalog.nativeExecutableHash,
    );
    await mkdir(join(root, "codex"));
    await mkdir(join(root, "config"));
    await writeFile(join(root, "mcp.json"), "{}");
    const notifications: Array<{ method: string; params: Wire.Json }> = [];
    const requests: NativeRequest[] = [];
    const start = () => {
      const child = spawn(
        process.execPath,
        [adapter, "app-server", "--listen", "stdio://"],
        {
          stdio: ["pipe", "pipe", "pipe"],
          windowsHide: true,
          env: {
            ...process.env,
            CODEX_HOME: join(root, "codex"),
            CLAUDE_CODEX_HOME: join(root, "adapter"),
            CLAUDE_CONFIG_DIR: join(root, "config"),
            CLAUDE_CODEX_MCP_SERVERS: join(root, "mcp.json"),
            CLAUDE_CODEX_MOCK: "1",
            CLAUDE_CODEX_DISABLE_CODEX_PROXY: "1",
            NODE_NO_WARNINGS: "1",
          },
        },
      );
      const rpc = new NativeRpc(catalog, child.stdin!, child.stdout!, {
        onClose: () => undefined,
        onRequest: (value) => {
          requests.push(value);
        },
        onNotification: (value) => {
          notifications.push(value);
        },
      });
      return { child, rpc };
    };
    let { child, rpc } = start();
    t.after(async () => {
      rpc.close();
      child.kill();
      if (child.exitCode === null) await once(child, "exit");
      await rm(root, {
        recursive: true,
        force: true,
        maxRetries: 10,
        retryDelay: 100,
      });
    });
    const call = async (method: string, params: Wire.Json) => {
      contract.validateInput(method, params);
      const reply = await rpc.request(method, params, {}, 15_000);
      assert.ok("result" in reply, `${method}: ${JSON.stringify(reply)}`);
      try {
        contract.validateResult(method, reply.result);
      } catch {
        const validator = new SchemaValidators().compile(
          contract.definition(method).outputSchema,
        );
        validator(reply.result);
        throw new Error(
          `${method} result differs from catalog: ${JSON.stringify(validator.errors?.[0])}`,
        );
      }
      return reply.result as Record<string, any>;
    };
    const waitFor = async <T>(
      items: T[],
      match: (value: T) => boolean,
    ): Promise<T> => {
      const deadline = Date.now() + 10_000;
      while (true) {
        const found = items.find(match);
        if (found) return found;
        assert.ok(
          rpc.connected && Date.now() < deadline,
          "expected native event was not observed",
        );
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
    };
    const init = await call("initialize", {
      clientInfo: {
        name: "ivy-agent-manager",
        title: "Ivy AgentManager",
        version: "test",
      },
      capabilities: { experimentalApi: true, requestAttestation: false },
    });
    assert.equal(init.codexHome, join(root, "codex"));
    assert.match(init.userAgent, /claude-codex/);
    rpc.notify("initialized");
    const started = await call("thread/start", { cwd: root, config: { model_reasoning_effort: "max" } });
    assert.equal(started.reasoningEffort, "max");
    const threadId = started.thread.id as string;
    const turn = await call("turn/start", {
      threadId,
      input: [{ type: "text", text: "Hello from the mock." }],
    });
    const turnId = turn.turn.id as string;
    await waitFor(
      notifications,
      (value) =>
        value.method === "turn/completed" &&
        (value.params as Record<string, any>)?.turn?.id === turnId,
    );
    assert.ok(
      notifications.some((value) => value.method === "item/agentMessage/delta"),
    );
    const turns = await call("thread/turns/list", { threadId });
    assert.ok(turns.data.some((value: { id: string }) => value.id === turnId));
    const items = await call("thread/items/list", {
      threadId,
      turnId,
      sortDirection: "desc",
      limit: 1,
    });
    assert.equal(items.data.length, 1);
    assert.equal(items.data[0].turnId, turnId);
    await call("thread/read", { threadId, includeTurns: true });
    await call("thread/list", { limit: 20 });
    await call("thread/loaded/list", {});
    await call("thread/resume", { threadId });
    await call("turn/interrupt", { threadId, turnId });
    await call("account/rateLimits/read", null);
    await call("model/list", {});
    rpc.close();
    child.kill();
    if (child.exitCode === null) await once(child, "exit");
    ({ child, rpc } = start());
    const restarted = await call("initialize", {
      clientInfo: {
        name: "ivy-agent-manager",
        title: "Ivy AgentManager",
        version: "test",
      },
      capabilities: { experimentalApi: true, requestAttestation: false },
    });
    assert.equal(restarted.codexHome, join(root, "codex"));
    rpc.notify("initialized");
    await call("thread/resume", { threadId });
    const persisted = await call("thread/items/list", { threadId, turnId });
    assert.ok(
      persisted.data.some((value: { turnId: string }) => value.turnId === turnId),
      "completed turn remains available after adapter restart",
    );
    const approvalThread = (
      await call("thread/start", {
        cwd: root,
        approvalPolicy: "on-request",
        sandbox: "workspace-write",
      })
    ).thread.id as string;
    for (const decision of ["decline", "accept"]) {
      const approvalTurn = (
        await call("turn/start", {
          threadId: approvalThread,
          input: [{ type: "text", text: "please run approval bash" }],
        })
      ).turn.id as string;
      const request = await waitFor(
        requests,
        (value) =>
          value.method === "item/commandExecution/requestApproval" &&
          (value.params as Record<string, unknown>)["turnId"] === approvalTurn,
      );
      const definition = catalog.serverRequests.find(
        (value) => value.method === request.method,
      )!;
      new SchemaValidators().validate(definition.inputSchema, request.params);
      new SchemaValidators().validate(definition.outputSchema, { decision });
      rpc.answer(
        request.method,
        request.id,
        { result: { decision } },
        () => undefined,
      );
      await waitFor(
        notifications,
        (value) =>
          value.method === "turn/completed" &&
          (value.params as Record<string, any>)?.turn?.id === approvalTurn,
      );
      assert.ok(
        notifications.some(
          (value) =>
            value.method === "serverRequest/resolved" &&
            (value.params as Record<string, unknown>)["requestId"] ===
              request.id,
        ),
      );
    }
    const questionTurn = (
      await call("turn/start", {
        threadId: approvalThread,
        input: [{ type: "text", text: "ask user question check" }],
      })
    ).turn.id as string;
    const question = await waitFor(
      requests,
      (value) =>
        value.method === "item/tool/requestUserInput" &&
        (value.params as Record<string, unknown>)["turnId"] === questionTurn,
    );
    const answer = { answers: { q0: { answers: ["OAuth"] } } };
    const questionDefinition = catalog.serverRequests.find(
      (value) => value.method === question.method,
    )!;
    const questionValidator = new SchemaValidators().compile(
      questionDefinition.inputSchema,
    );
    if (!questionValidator(question.params))
      throw new Error(
        `question request differs from catalog: ${JSON.stringify(questionValidator.errors?.[0])}; ${JSON.stringify(question.params).slice(0, 1600)}`,
      );
    new SchemaValidators().validate(questionDefinition.outputSchema, answer);
    rpc.answer(
      question.method,
      question.id,
      { result: answer },
      () => undefined,
    );
    await waitFor(
      notifications,
      (value) =>
        value.method === "turn/completed" &&
        (value.params as Record<string, any>)?.turn?.id === questionTurn,
    );
    const interruptedTurn = (
      await call("turn/start", {
        threadId: approvalThread,
        input: [
          {
            type: "text",
            text: "active interruption check " + "x".repeat(400),
          },
        ],
      })
    ).turn.id as string;
    await waitFor(
      notifications,
      (value) =>
        value.method === "item/agentMessage/delta" &&
        (value.params as Record<string, unknown>)["turnId"] === interruptedTurn,
    );
    const steer = await call("turn/steer", {
      threadId: approvalThread,
      expectedTurnId: interruptedTurn,
      input: [{ type: "text", text: "Follow this updated instruction." }],
    });
    assert.equal(steer.turnId, interruptedTurn);
    await call("turn/interrupt", {
      threadId: approvalThread,
      turnId: interruptedTurn,
    });
    const interrupted = await waitFor(
      notifications,
      (value) =>
        value.method === "turn/completed" &&
        (value.params as Record<string, any>)?.turn?.id === interruptedTurn,
    );
    assert.equal(
      (interrupted.params as Record<string, any>).turn.status,
      "interrupted",
    );
    const forbidden = await rpc.request(
      "thread/start",
      { cwd: root, model: "gpt-6-sol" },
      {},
      15_000,
    );
    assert.ok("error" in forbidden);
    const missing = await rpc.request(
      "thread/read",
      { threadId: "missing", includeTurns: false },
      {},
      15_000,
    );
    assert.ok("error" in missing);
    assert.equal(rpc.connected, true);
  },
);
