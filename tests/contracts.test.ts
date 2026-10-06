import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  canonical,
  digest,
  hashJson,
  compareVersions,
} from "../packages/contracts/src/canonical.js";
import {
  operationSchema,
  operations,
  validateRequest,
  validateInput,
  validateOutput,
  validateComponent,
} from "../packages/contracts/src/validation.js";
import { bundledSchema as bundledCoreSchema } from "../packages/contracts/src/core-bundle.js";

test("canonical byte limits remain exact for ASCII, escaped controls, Unicode and surrogate boundaries", () => {
  const strings = [
    "plain ASCII",
    '\u0000\n\t"\\',
    "Straße 日本語",
    "😀",
    "\ud800",
    "\udfff",
    "\ud800A\udfff",
  ];
  for (let point = 0; point <= 0x10ffff; point += 997)
    strings.push(String.fromCodePoint(point));
  for (const value of strings) {
    const input = { [value]: [value, true, null, 123] },
      encoded = canonical(input),
      bytes = Buffer.byteLength(encoded, "utf8");
    assert.equal(canonical(input, bytes), encoded);
    assert.throws(() => canonical(input, bytes - 1), {
      code: "content_too_large",
    });
  }
});

test("all normative wire examples validate through the canonical catalog", () => {
  for (const name of ["object-write", "object-query", "tool-call"]) {
    const request: unknown = JSON.parse(
      readFileSync(`specs/examples/${name}.json`, "utf8"),
    );
    validateRequest(request);
    validateInput(request.method, request.params);
  }
  validateComponent(
    JSON.parse(readFileSync("specs/examples/component.json", "utf8")),
  );
  assert.equal(operations["hosts.report"]!.access, "service");
  assert.equal(operations["hosts.observations"]!.access, "client");
  for (const name of [
    "serviceNodes.contracts",
    "uis.catalog",
    "uis.releases",
    "uis.inspect",
    "discovery.list",
    "discovery.describe",
    "discovery.call",
    "topics.list",
    "notifications.subscribe",
    "packages.catalog",
  ]) {
    assert.equal(operations[name]!.access, "client");
    assert.equal(operations[name]!.mutation, false);
  }
  assert.equal(operations["events.unsubscribe"]!.access, "service");
  assert.equal(operations["events.unsubscribe"]!.mutation, true);
  assert.equal(operations["events.head"]!.access, "service");
  assert.equal(operations["events.head"]!.mutation, false);
  assert.equal(operations["objects.delete"]!.discoverable, false);
});

test("every core MCP operation bundles its complete input and output schema", () => {
  for (const definition of Object.values(operations)) {
    assert.doesNotThrow(() =>
      bundledCoreSchema(definition.input, String(operationSchema["$id"])),
    );
    assert.doesNotThrow(() =>
      bundledCoreSchema(definition.output, String(operationSchema["$id"])),
    );
  }
});

test("malformed envelopes and mutation input variants are rejected", () => {
  for (const value of [
    null,
    [],
    0,
    true,
    "x",
    {},
    { jsonrpc: "2.0", method: "objects.write", params: {} },
    { jsonrpc: "2.0", id: 1, method: "x", params: [] },
    { jsonrpc: "2.0", id: 2 ** 53, method: "x", params: {} },
    { jsonrpc: "2.0", id: -1, method: "x", params: {} },
    { jsonrpc: "2.0", id: 1, method: "x".repeat(193), params: {} },
    { jsonrpc: "2.0", id: 1, method: "x", params: {}, extra: true },
  ]) {
    assert.throws(() => validateRequest(value));
  }
  const base = {
    mutationId: "m",
    contractVersion: "1.0.0",
    content: { encoding: "text", value: "hello" },
    create: { contractKey: "wiki/page", parentId: null, name: "a" },
  };
  for (const value of [
    { ...base, contractVersion: "latest" },
    { ...base, objectId: "o", expectedRevision: 1 },
    { ...base, create: undefined, objectId: "o" },
    { ...base, content: { encoding: "base64", value: "a===" } },
  ]) {
    assert.throws(() => validateInput("objects.write", value));
  }
  assert.throws(() =>
    validateInput("tools.call", {
      qualifiedName: "codex.turn/start",
      arguments: {},
    }),
  );
  assert.throws(() => validateInput("not.implemented", {}));
  const write = { mutationId: "rename", contractVersion: "1.0.0", references: {}, content: { encoding: "text", value: "Body" } };
  validateInput("objects.write", { ...write, objectId: "page", expectedRevision: 1, name: "Renamed" });
  assert.throws(() => validateInput("objects.write", { ...write, create: { contractKey: "wiki/page", parentId: null, ownerObjectId: null, name: "Created" }, name: "Ambiguous" }));
});

test("names preserve native separators and case, exact hash is mandatory", () => {
  validateInput("tools.call", {
    qualifiedName: "codex.account/login/start",
    arguments: {},
    expectedDefinitionHash: "sha256:" + "f".repeat(64),
  });
  validateInput("tools.call", {
    qualifiedName: "codex.mcpServer/elicitation/complete",
    arguments: {},
    expectedDefinitionHash: "sha256:" + "f".repeat(64),
  });
});

test("operation validation excludes prototype names and keeps input/output validators separate", () => {
  for (const method of ["toString", "constructor", "__proto__"]) {
    assert.throws(() => validateInput(method, {}), { code: "not_found" });
    assert.throws(() => validateOutput(method, {}), { code: "internal_error" });
  }
  // Both the first call and the cached call must apply their distinct exact contract.
  for (let attempt = 0; attempt < 2; attempt++) {
    validateInput("system.status", {});
    assert.throws(() => validateInput("system.status", { ready: true }), {
      code: "invalid_arguments",
    });
    assert.throws(() => validateOutput("system.status", {}), {
      code: "internal_error",
    });
  }
});

test("typed substring query accepts literal strings and rejects extra or unbounded operands", () => {
  const query = (value: unknown) => ({
    contractKey: "task-board/task",
    where: { op: "contains", field: "data:/fields/title", value },
  });
  for (const value of ["", "%_'\\ Straße 日本語", "🧪".repeat(4096)])
    validateInput("objects.query", query(value));
  for (const value of [null, false, 1, [], {}, "x".repeat(4097)])
    assert.throws(() => validateInput("objects.query", query(value)));
  const valid = query("title");
  assert.throws(() =>
    validateInput("objects.query", {
      ...valid,
      where: { ...valid.where, caseSensitive: false },
    }),
  );
  assert.throws(() =>
    validateInput("objects.query", {
      ...valid,
      where: { ...valid.where, field: "data.fields.title" },
    }),
  );
});

test("ownerObjectId is a canonical nullable query field", () => {
  const base = {
    contractKey: "service/artifact",
    select: ["object.ownerObjectId"],
    orderBy: [{ field: "object.ownerObjectId", direction: "asc" }],
  };
  for (const where of [
    { op: "eq", field: "object.ownerObjectId", value: "owner-1" },
    { op: "in", field: "object.ownerObjectId", value: ["owner-1", null] },
    { op: "isNull", field: "object.ownerObjectId" },
  ])
    validateInput("objects.query", { ...base, where });
  assert.throws(() =>
    validateInput("objects.query", {
      ...base,
      select: ["object.lifecycleOwnerId"],
    }),
  );
});

test("canonical hash vectors include integer-like keys, UTF-16 ordering and negative zero", () => {
  assert.equal(
    canonical({ "2": 2, "10": 10, z: -0, a: [true, null, "é"] }),
    '{"10":10,"2":2,"a":[true,null,"é"],"z":0}',
  );
  assert.equal(canonical({ "\uE000": 1, "\u{10000}": 2 }), '{"𐀀":2,"":1}');
  assert.equal(
    digest(""),
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  );
  assert.equal(hashJson({ b: 2, a: 1 }), digest('{"a":1,"b":2}'));
  assert.equal(hashJson({ zero: -0 }), hashJson({ zero: 0 }));
  assert.notEqual(hashJson([1, 2]), hashJson([2, 1]));
});

test("non-JSON runtime values cannot collide with valid operation arguments", () => {
  const cycle: Record<string, unknown> = {};
  cycle["self"] = cycle;
  let getterCalled = false;
  const getter = {
    get unsafe() {
      getterCalled = true;
      return 1;
    },
  };
  for (const value of [
    undefined,
    NaN,
    Infinity,
    2 ** 53,
    1n,
    new Date(),
    new Map(),
    () => 1,
    { a: undefined },
    [undefined],
    Array(2),
    cycle,
    getter,
    { [Symbol("x")]: 1 },
  ])
    assert.throws(() => canonical(value));
  assert.equal(getterCalled, false);
  assert.throws(() => canonical("x".repeat(100), 10));
  assert.equal(compareVersions("10.0.0", "2.999.0"), 1);
  assert.equal(compareVersions("1.0.0", "1.0.0"), 0);
  assert.throws(() => compareVersions("01.0.0", "1.0.0"));
});
