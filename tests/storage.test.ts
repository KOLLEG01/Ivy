import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { HiveStore } from "../services/hive/src/store.js";
import { STORAGE_FORMAT } from "../services/hive/src/storage-schema.js";
import { Objects } from "../services/hive/src/objects.js";
import { Events } from "../services/hive/src/events.js";
import { queryObjects } from "../services/hive/src/query.js";
import { canonical, digest } from "../packages/contracts/src/canonical.js";
import type { Json } from "../packages/contracts/src/canonical.js";
import { SchemaValidators } from "../packages/contracts/src/schema.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import { operationId } from "../packages/contracts/src/operation-id.js";
import {
  mutationReceiptCount,
  mutationReceiptJournalBytes,
  mutationReceiptResultBytes,
} from "../packages/contracts/src/limits.js";
import {
  inspectStoppedStorage,
  storageInspection,
} from "../services/hive/src/inspect-storage.js";
import { readFileSync } from "node:fs";
import type {
  DataContract,
  ObjectWrite,
} from "../packages/contracts/src/types.js";
import { Registry } from "../services/hive/src/registry.js";
import type { Wire } from "../packages/contracts/src/generated.js";

const credential = {
  principalId: "test-principal",
  digest: digest("synthetic-test-credential"),
};
const context = {
  credentialDigest: credential.digest,
  principalId: credential.principalId,
};
test("UI invalidations roll back and include objects excluded from the durable journal", (t) => {
  const store = new HiveStore(":memory:");
  t.after(() => store.close());
  store.changes.clear();
  store.transaction(() => store.appendCoreEvent(context, "execution", 1, "write", "transient", new Date().toISOString(), "secretary/execution"));
  assert.deepEqual([...store.changes], ["objects/secretary/execution"]);
  assert.equal(new Events(store).head().throughSequence, 0);
  assert.throws(() => store.transaction(() => {
    store.invalidate("objects/wiki/page");
    throw new Error("rollback");
  }), /rollback/);
  assert.deepEqual([...store.changes], ["objects/secretary/execution"]);
});
const textContract: DataContract = {
  key: "test/text",
  version: "1.0.0",
  owner: { kind: "agent" },
  mediaType: "text/markdown",
  retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
  specMarkdown: "Isolated test text.",
};
function fixture(t: TestContext) {
  const directory = mkdtempSync(join(tmpdir(), "ivy-hive-test-"));
  const store = new HiveStore(join(directory, "hive.sqlite"));
  store.configureCredentials([credential]);
  const issuedAt = Date.now(),
    ids = new Map<string, string>();
  const oid = (value: string) => {
    let id = ids.get(value);
    if (!id) {
      id = operationId(store.runtimeEpoch, issuedAt, digest(value).slice(7));
      ids.set(value, id);
    }
    return id;
  };
  const rawObjects = new Objects(store);
  const objects = new Proxy(rawObjects, {
    get(target, property) {
      const member = Reflect.get(target, property) as unknown;
      if (typeof member !== "function") return member;
      if (
        !["registerAgent", "write", "move", "reorder", "archive", "delete"].includes(
          String(property),
        )
      )
        return (member as (...args: unknown[]) => unknown).bind(target);
      return (owner: unknown, params: { mutationId: string }) =>
        (member as (owner: unknown, params: unknown) => unknown).call(
          target,
          owner,
          { ...params, mutationId: oid(params.mutationId) },
        );
    },
  }) as Objects;
  const events = new Events(store);
  objects.registerAgent(context, {
    definition: textContract,
    mutationId: "register-text",
  });
  t.after(() => {
    if (store.db.isOpen) store.close();
    assert.ok(
      resolve(directory).startsWith(resolve(tmpdir()) + "\\") ||
        resolve(directory).startsWith(resolve(tmpdir()) + "/"),
    );
    assert.ok(directory.includes("ivy-hive-test-"));
    rmSync(directory, { recursive: true, force: true });
  });
  const writeText = (
    name: string,
    text = "hello",
    parentId: string | null = null,
  ) =>
    objects.write(context, {
      mutationId: randomUUID(),
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: textContract.key,
        name,
        parentId,
        ownerObjectId: null,
      },
      content: { encoding: "text", value: text },
    });
  return { store, objects, events, directory, writeText, oid };
}
const hasCode = (code: string) => (error: unknown) =>
  error instanceof IvyError && error.code === code;

test("query documents preserve revision pins and paginate within the full response byte bound", (t) => {
  const { objects, store, writeText } = fixture(t);
  const root = writeText("query-document-root").object.id;
  const contract: DataContract = {
    ...textContract,
    key: "test/query-documents",
    mediaType: "application/json",
    jsonSchema: {
      type: "object",
      properties: { index: { type: "integer" }, payload: { type: "string" } },
      required: ["index", "payload"],
      additionalProperties: false,
    },
  };
  objects.registerAgent(context, {
    definition: contract,
    mutationId: "query-documents-contract",
  });
  for (let index = 0; index < 3; index++)
    objects.write(context, {
      mutationId: "query-document-" + index,
      contractVersion: "1.0.0",
      references: { root: { objectId: root, revision: 1 } },
      create: {
        contractKey: contract.key,
        parentId: root,
        ownerObjectId: null,
        name: "document-" + index,
      },
      content: {
        encoding: "json",
        value: { index, payload: "x".repeat(900_000) },
      },
    });
  const request = {
    contractKey: contract.key,
    includeContent: true,
    select: ["data:/index"],
    orderBy: [{ field: "data:/index", direction: "asc" as const }],
    limit: 50,
  };
  const first = queryObjects(store, request);
  assert.equal(
    first.items.length,
    2,
    "documents, rather than only projections, consume the page budget",
  );
  assert.ok(Buffer.byteLength(canonical(first)) < 2 * 1024 * 1024);
  assert.ok(first.nextCursor);
  const last = queryObjects(store, { ...request, cursor: first.nextCursor });
  assert.equal(last.items.length, 1);
  assert.equal(last.nextCursor, null);
  for (const item of [...first.items, ...last.items]) {
    assert.deepEqual(
      item.document,
      objects.read({ objectId: item.objectId, revision: item.revision }),
    );
    assert.equal(item.document!.revision.contractVersion, item.contractVersion);
  }
  assert.deepEqual(
    [...first.items, ...last.items].map((item) => item.values["data:/index"]),
    [0, 1, 2],
  );
  assert.throws(
    () =>
      queryObjects(store, {
        ...request,
        includeContent: false,
        cursor: first.nextCursor!,
      }),
    hasCode("invalid_cursor"),
  );
  const pinned = first.items[0]!.document!;
  objects.write(context, {
    mutationId: "query-document-update",
    objectId: pinned.object.id,
    expectedRevision: pinned.revision.revision,
    contractVersion: "1.0.0",
    references: pinned.revision.references,
    content: { encoding: "json", value: { index: 0, payload: "updated" } },
  });
  const historical = objects.read({
    objectId: pinned.object.id,
    revision: pinned.revision.revision,
  });
  assert.deepEqual(pinned.revision, historical.revision);
  assert.deepEqual(pinned.content, historical.content);
  assert.equal(
    queryObjects(store, { contractKey: contract.key }).items[0]!.document,
    undefined,
    "projections remain the default",
  );
  assert.throws(
    () =>
      queryObjects(store, {
        contractKey: textContract.key,
        includeContent: true,
      }),
    hasCode("invalid_arguments"),
  );
});

test("content writes rename atomically with CAS, references and exact replay", (t) => {
  const { objects, store, writeText } = fixture(t);
  const page = writeText("Original", "Original text");
  const child = writeText("Child", "Child text", page.object.id);
  writeText("Occupied");
  const update: ObjectWrite = {
    mutationId: "rename-and-write", objectId: page.object.id, expectedRevision: 1,
    contractVersion: "1.0.0", name: "Renamed",
    references: { child: { objectId: child.object.id, revision: 1 } },
    content: { encoding: "text", value: "New text" },
  };
  store.changes.clear();
  const saved = objects.write(context, update);
  assert.ok(store.changes.has("objects"), "renames invalidate descendant paths across contracts");
  store.changes.clear();
  assert.equal(saved.object.name, "Renamed");
  assert.equal(saved.revision.revision, 2);
  assert.deepEqual(saved.revision.references, update.references);
  assert.equal(objects.stat({ objectId: child.object.id }).path, "/Renamed/Child");
  assert.throws(() => objects.write(context, { ...update, mutationId: "stale", name: "Stale" }), hasCode("revision_conflict"));
  assert.throws(() => objects.write(context, { ...update, mutationId: "occupied", expectedRevision: 2, name: "Occupied" }), hasCode("revision_conflict"));
  assert.throws(() => objects.write(context, { ...update, mutationId: "bad-reference", expectedRevision: 2, name: "Rolled back", references: { missing: { objectId: "missing", revision: 1 } } }), hasCode("not_found"));
  assert.equal(store.changes.size, 0, "failed atomic renames do not publish changes");
  assert.equal(objects.stat({ objectId: child.object.id }).path, "/Renamed/Child");
  assert.deepEqual(objects.read({ objectId: page.object.id }), { ...saved, content: update.content });
  objects.write(context, { ...update, mutationId: "later", expectedRevision: 2, name: "Later", content: { encoding: "text", value: "Later text" } });
  assert.deepEqual(objects.write(context, update), saved);
  assert.equal(objects.stat({ objectId: page.object.id }).name, "Later");
  assert.equal(objects.stat({ objectId: child.object.id }).path, "/Later/Child");
  store.changes.clear();
  objects.write(context, { ...update, mutationId: "content-only", expectedRevision: 3, name: "Later" });
  assert.deepEqual([...store.changes], ["objects/test/text"], "unchanged names retain narrow content invalidations");
});

test("permanent deletion removes Wiki subtrees, attachments and every revision atomically", (t) => {
  const { objects, store, writeText } = fixture(t);
  const wiki: DataContract = { ...textContract, key: "wiki/page" };
  const attachment: DataContract = {
    ...textContract,
    key: "wiki/attachment",
    retention: { objects: { mode: "owned" }, revisions: { mode: "all" } },
  };
  objects.registerAgent(context, { definition: wiki, mutationId: "register-wiki" });
  objects.registerAgent(context, { definition: attachment, mutationId: "register-attachment" });
  const page = (name: string, parentId: string | null = null) => objects.write(context, {
    mutationId: "create-" + name,
    contractVersion: "1.0.0",
    references: {},
    create: { contractKey: "wiki/page", name, parentId, ownerObjectId: null },
    content: { encoding: "text", value: name },
  });
  const root = page("root"), child = page("child", root.object.id), sibling = page("sibling");
  const updated = objects.write(context, {
    mutationId: "update-child",
    objectId: child.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: {},
    content: { encoding: "text", value: "new secret text" },
  });
  const file = objects.write(context, {
    mutationId: "create-file",
    contractVersion: "1.0.0",
    references: {},
    create: { contractKey: "wiki/attachment", name: "file", parentId: child.object.id, ownerObjectId: child.object.id },
    content: { encoding: "text", value: "file bytes" },
  });
  const external = objects.write(context, {
    mutationId: "external-reference",
    objectId: sibling.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: { child: { objectId: child.object.id, revision: 1 } },
    content: { encoding: "text", value: "external" },
  });
  const unused = objects.write(context, {
    mutationId: 'unused-file', contractVersion: '1.0.0', references: {},
    create: { contractKey: 'wiki/attachment', name: 'unused', parentId: child.object.id, ownerObjectId: child.object.id },
    content: { encoding: 'text', value: 'unused bytes' },
  });
  const removeUnused = { objectId: unused.object.id, expectedRevision: 1, mutationId: 'delete-unused-file' };
  assert.deepEqual(objects.delete(context, removeUnused), { deleted: true });
  assert.deepEqual(objects.delete(context, removeUnused), { deleted: true });
  assert.equal(objects.stat({ objectId: child.object.id }).currentRevision, 2);
  const pinned = objects.write(context, {
    mutationId: 'pin-file', objectId: child.object.id, expectedRevision: 2, contractVersion: '1.0.0',
    references: { file: { objectId: file.object.id, revision: 1 } }, content: { encoding: 'text', value: 'with attachment' },
  });
  objects.write(context, {
    mutationId: 'remove-current-file-reference', objectId: child.object.id, expectedRevision: pinned.revision.revision,
    contractVersion: '1.0.0', references: {}, content: { encoding: 'text', value: 'attachment removed from current text' },
  });
  assert.throws(() => objects.delete(context, {
    objectId: file.object.id, expectedRevision: 1, mutationId: 'delete-historically-pinned-file',
  }), hasCode('target_conflict'));
  assert.throws(() => objects.delete(context, {
    objectId: root.object.id, expectedRevision: 1, mutationId: "blocked-delete",
  }), hasCode("target_conflict"));
  assert.equal(objects.stat({ objectId: child.object.id }).currentRevision, updated.revision.revision + 2);
  objects.delete(context, {
    objectId: sibling.object.id, expectedRevision: external.object.currentRevision, mutationId: "delete-external",
  });
  const result = objects.delete(context, {
    objectId: root.object.id, expectedRevision: 1, mutationId: "delete-root",
  });
  assert.deepEqual(result, { deleted: true });
  assert.deepEqual(objects.delete(context, {
    objectId: root.object.id, expectedRevision: 1, mutationId: "delete-root",
  }), result);
  for (const id of [root.object.id, child.object.id, file.object.id, sibling.object.id]) {
    assert.throws(() => objects.stat({ objectId: id }), hasCode("not_found"));
    assert.equal(store.get("SELECT 1 FROM revisions WHERE object_id=?", id), undefined);
    assert.equal(store.get("SELECT 1 FROM object_fts WHERE object_id=?", id), undefined);
  }
  assert.equal(store.get("SELECT 1 FROM revision_references LIMIT 1"), undefined);
  assert.equal(objects.list({ parentId: null }).items.length, 0);
  assert.equal(writeText("fresh").object.position, 0);
});

for (const deleteWorkspace of [false, true])
test(`permanent task deletion ${deleteWorkspace ? "through its workspace" : "directly"} removes workflow records and keeps other workspaces`, (t) => {
  const { objects, store, writeText } = fixture(t);
  const contract = (key: string): DataContract => ({
    key,
    version: "1.0.0",
    owner: { kind: "agent" },
    mediaType: "application/json",
    retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
    specMarkdown: key,
    jsonSchema: { type: "object", additionalProperties: true },
  });
  for (const key of ["task-board/task", "task-board/run", "task-board/result", "task-board/task-key-allocation"])
    objects.registerAgent(context, { definition: contract(key), mutationId: "register-" + key });
  objects.registerAgent(context, { definition: { ...textContract, key: "wiki/page" }, mutationId: "register-page" });
  const root = objects.write(context, {
    mutationId: "make-root", contractVersion: "1.0.0", references: {},
    create: { contractKey: "wiki/page", name: "task-board-root", parentId: null, ownerObjectId: null },
    content: { encoding: "text", value: "Workspace" },
  }), otherRoot = writeText("other-root");
  const make = (key: string, name: string, parentId: string, value: Record<string, Json>) => objects.write(context, {
    mutationId: "make-" + name,
    contractVersion: "1.0.0",
    references: {},
    create: { contractKey: key, name, parentId, ownerObjectId: null },
    content: { encoding: "json", value },
  });
  const task = make("task-board/task", "TASK-0001", root.object.id, { taskKey: "TASK-0001", claim: { active: true }, publication: null });
  assert.throws(() => objects.delete(context, {
    objectId: task.object.id, expectedRevision: 1, mutationId: "active-delete",
  }), hasCode("target_conflict"));
  assert.throws(() => objects.delete(context, {
    objectId: root.object.id, expectedRevision: 1, mutationId: "active-subtree-delete",
  }), hasCode("target_conflict"));
  const run = make("task-board/run", "run", root.object.id, { taskId: task.object.id });
  const result = make("task-board/result", "result", root.object.id, { taskId: task.object.id });
  const allocation = make("task-board/task-key-allocation", "allocation", root.object.id, { taskKey: "TASK-0001" });
  const otherAllocation = make("task-board/task-key-allocation", "other-allocation", otherRoot.object.id, { taskKey: "TASK-0001" });
  const updated = objects.write(context, {
    mutationId: "finish-task",
    objectId: task.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: { result: { objectId: result.object.id, revision: 1 } },
    content: { encoding: "json", value: { taskKey: "TASK-0001", claim: null, publication: null } },
  });
  const dependent = make("task-board/task", "TASK-0002", root.object.id, {
    taskKey: "TASK-0002", claim: null, publication: null,
    fields: { dependencies: [task.object.id] },
  });
  assert.throws(() => objects.delete(context, {
    objectId: task.object.id, expectedRevision: 1, mutationId: "stale-delete",
  }), hasCode("revision_conflict"));
  assert.throws(() => objects.delete(context, {
    objectId: task.object.id, expectedRevision: updated.object.currentRevision, mutationId: "dependency-delete",
  }), hasCode("target_conflict"));
  if (deleteWorkspace) objects.delete(context, {
    objectId: root.object.id, expectedRevision: 1, mutationId: "delete-workspace",
  });
  else {
    objects.delete(context, { objectId: dependent.object.id, expectedRevision: 1, mutationId: "delete-dependent" });
    objects.delete(context, { objectId: task.object.id, expectedRevision: updated.object.currentRevision, mutationId: "delete-task" });
    assert.equal(objects.stat({ objectId: root.object.id }).id, root.object.id);
  }
  for (const id of [...(deleteWorkspace ? [root.object.id] : []), task.object.id, dependent.object.id, run.object.id, result.object.id, allocation.object.id]) {
    assert.equal(store.get("SELECT 1 FROM objects WHERE id=?", id), undefined);
    assert.equal(store.get("SELECT 1 FROM revisions WHERE object_id=?", id), undefined);
  }
  assert.equal(objects.stat({ objectId: otherAllocation.object.id }).name, "other-allocation");
});

test("retained format 2 keeps dormant legacy columns without exposing them as runtime authority", (t) => {
  const { store } = fixture(t);
  assert.deepEqual(
    store.all("PRAGMA table_info(service_nodes)").map((row) => row["name"]),
    [
      "id",
      "principal_id",
      "host_id",
      "service_name",
      "generation",
      "status_json",
    ],
  );
  assert.deepEqual(
    store.all("PRAGMA table_info(subscriptions)").map((row) => row["name"]),
    [
      "node_id",
      "name",
      "filter_json",
      "initial_sequence",
      "acknowledged_sequence",
      "batch_generation",
      "batch_json",
    ],
  );
  assert.equal(
    store.get(
      "SELECT name FROM sqlite_master WHERE type='table' AND name='tool_definitions'",
    )!["name"],
    "tool_definitions",
  );
});

test("an existing Hive journal gains durable subscription markers without a storage reset", (t) => {
  const { store, directory, writeText } = fixture(t);
  writeText("before-marker");
  const count = Number(store.get("SELECT COUNT(*) AS count FROM events")!["count"]);
  store.run("DROP TABLE durable_subscriptions");
  store.close();
  const reopened = new HiveStore(join(directory, "hive.sqlite"));
  try {
    assert.equal(Number(reopened.get("SELECT COUNT(*) AS count FROM events")!["count"]), count);
    assert.equal(String(reopened.get("SELECT name FROM sqlite_master WHERE type='table' AND name='durable_subscriptions'")!["name"]), "durable_subscriptions");
    assert.equal(Number(reopened.get("PRAGMA user_version")!["user_version"]), STORAGE_FORMAT);
  } finally { reopened.close(); }
});

test("registry and live/stopped inspection share scoped descendants, archives and exact external evidence", (t) => {
  const { store, objects, writeText, directory } = fixture(t),
    oldRoot = writeText("old-area"),
    newRoot = writeText("new-area");
  objects.registerAgent(context, {
    definition: { ...textContract, version: "2.0.0" },
    mutationId: "scope-v2",
  });
  const old = writeText("old-evidence", "old", oldRoot.object.id),
    nested = writeText("nested", "old", newRoot.object.id);
  for (const row of [newRoot, nested])
    objects.write(context, {
      mutationId: randomUUID(),
      objectId: row.object.id,
      expectedRevision: 1,
      contractVersion: "2.0.0",
      references: {},
      content: { encoding: "text", value: "new" },
    });
  objects.archive(context, {
    mutationId: randomUUID(),
    objectId: oldRoot.object.id,
    archived: true,
  });
  const scope: Wire.ContractReadScope = {
    roots: [newRoot.object.id],
    history: "current",
    includeArchived: false,
    references: [],
  };
  const required = {
      key: textContract.key,
      readVersions: ["2.0.0"],
      writeVersions: ["2.0.0"],
      readScope: scope,
    },
    registry = new Registry(store);
  assert.doesNotThrow(() => registry.checkRequirements([required]));
  assert.doesNotThrow(() => registry.checkRequirements([{ ...required, readVersions: ["1.0.0", "2.0.0", "3.0.0"] }]));
  assert.throws(() => registry.checkRequirements([{ ...required, writeVersions: ["3.0.0"] }]), hasCode("contract_not_found"));
  assert.deepEqual(
    storageInspection(store.db, [required]).contracts[0]!.versions,
    ["2.0.0"],
  );
  assert.throws(
    () =>
      registry.checkRequirements([
        { ...required, readScope: { ...scope, history: "all" } },
      ]),
    hasCode("contract_version_conflict"),
  );
  assert.throws(
    () =>
      registry.checkRequirements([
        {
          ...required,
          readScope: {
            ...scope,
            references: [{ objectId: old.object.id, revision: 1 }],
          },
        },
      ]),
    hasCode("contract_version_conflict"),
  );
  assert.throws(
    () =>
      registry.checkRequirements([
        { key: required.key, readVersions: ["2.0.0"], writeVersions: [] },
      ]),
    hasCode("contract_version_conflict"),
  );
  assert.deepEqual(
    storageInspection(store.db, [
      {
        key: required.key,
        readScope: { ...scope, roots: [oldRoot.object.id] },
      },
    ]).contracts[0]!.versions,
    [],
  );
  assert.deepEqual(
    storageInspection(store.db, [
      {
        key: required.key,
        readScope: {
          ...scope,
          roots: [oldRoot.object.id],
          includeArchived: true,
        },
      },
    ]).contracts[0]!.versions,
    ["1.0.0"],
  );
  assert.throws(
    () =>
      registry.checkRequirements([
        { ...required, readScope: { ...scope, roots: ["missing"] } },
      ]),
    hasCode("not_found"),
  );
  const prepare = store.db.prepare.bind(store.db);
  const statements = t.mock.method(store.db, "prepare", (sql: string) => {
    assert.doesNotMatch(sql, /JOIN objects|r\.content/,
      "unrestricted compatibility uses version metadata without object joins or payload reads");
    return prepare(sql);
  });
  assert.deepEqual(storageInspection(store.db, [{ key: required.key }]).contracts[0]!.versions, ["1.0.0", "2.0.0"]);
  assert.deepEqual(storageInspection(store.db, [{ key: required.key, readScope: {
    roots: null, history: "all", includeArchived: true, references: [{ objectId: old.object.id, revision: 1 }],
  } }]).contracts[0]!.versions, ["1.0.0", "2.0.0"]);
  assert.throws(() => storageInspection(store.db, [{ key: required.key, readScope: {
    roots: null, history: "all", includeArchived: true, references: [{ objectId: old.object.id, revision: 999 }],
  } }]), hasCode("not_found"));
  statements.mock.restore();
  const expected = storageInspection(store.db, [required]);
  store.close();
  assert.deepEqual(
    inspectStoppedStorage(join(directory, "hive.sqlite"), [required]),
    expected,
  );
});

test("typed queries accept allOf number/integer fields and reject fractional operands", (t) => {
  const { store, objects } = fixture(t),
    contract: DataContract = {
      ...textContract,
      key: "test/intersection",
      mediaType: "application/json",
      jsonSchema: {
        type: "object",
        properties: {
          value: { allOf: [{ type: "number" }, { type: "integer" }] },
        },
        required: ["value"],
        additionalProperties: false,
      },
    };
  objects.registerAgent(context, {
    definition: contract,
    mutationId: "integer-intersection",
  });
  const row = objects.write(context, {
    mutationId: randomUUID(),
    contractVersion: "1.0.0",
    references: {},
    create: {
      contractKey: contract.key,
      name: "integer",
      parentId: null,
      ownerObjectId: null,
    },
    content: { encoding: "json", value: { value: 7 } },
  });
  const query = {
    contractKey: contract.key,
    contractVersions: ["1.0.0"],
    select: ["data:/value"],
  };
  assert.equal(
    queryObjects(store, {
      ...query,
      where: { op: "eq", field: "data:/value", value: 7 },
    }).items[0]?.objectId,
    row.object.id,
  );
  assert.throws(
    () =>
      queryObjects(store, {
        ...query,
        where: { op: "eq", field: "data:/value", value: 7.5 },
      }),
    hasCode("invalid_arguments"),
  );
});

test("Hive-owned inspection retains historical versions, refuses a live owner and never changes stopped database bytes", (t) => {
  const { store, objects, writeText, directory } = fixture(t);
  const first = writeText("history");
  objects.registerAgent(context, {
    definition: { ...textContract, version: "2.0.0" },
    mutationId: "inspect-v2",
  });
  objects.write(context, {
    mutationId: "upgrade-inspection",
    objectId: first.object.id,
    expectedRevision: 1,
    contractVersion: "2.0.0",
    references: {},
    content: { encoding: "text", value: "current" },
  });
  const expected = {
    schemaVersion: 1,
    exists: true,
    format: STORAGE_FORMAT,
    contracts: [
      { key: textContract.key, versions: ["1.0.0", "2.0.0"] },
      { key: "not/stored", versions: [] },
    ],
  };
  assert.deepEqual(
    storageInspection(store.db, [
      { key: textContract.key },
      { key: "not/stored" },
    ]),
    expected,
  );
  const file = join(directory, "hive.sqlite");
  assert.throws(
    () => inspectStoppedStorage(file, [{ key: textContract.key }]),
    /locked/,
  );
  store.close();
  const before = digest(readFileSync(file));
  assert.deepEqual(
    inspectStoppedStorage(file, [
      { key: textContract.key },
      { key: "not/stored" },
    ]),
    expected,
  );
  assert.equal(digest(readFileSync(file)), before);
  assert.deepEqual(inspectStoppedStorage(join(directory, "fresh.sqlite"), []), {
    schemaVersion: 1,
    exists: false,
    format: 0,
    contracts: [],
  });
});

test("an unsupported actual Hive database format is refused by offline inspection and the previous reader without rewriting it", (t) => {
  const { store, writeText, directory } = fixture(t);
  writeText("retained-before-format-change");
  store.close();
  const file = join(directory, "hive.sqlite"),
    future = new DatabaseSync(file);
  try {
    future.exec(`PRAGMA user_version=${STORAGE_FORMAT + 1};`);
  } finally {
    future.close();
  }
  const before = digest(readFileSync(file));
  assert.throws(
    () => inspectStoppedStorage(file, [{ key: textContract.key }]),
    hasCode("unsupported_storage_format"),
  );
  assert.throws(
    () => new HiveStore(file),
    hasCode("unsupported_storage_format"),
  );
  assert.equal(digest(readFileSync(file)), before);
  const retained = new DatabaseSync(file, { readOnly: true });
  try {
    assert.equal(
      retained.prepare("PRAGMA user_version").get()!["user_version"],
      STORAGE_FORMAT + 1,
    );
    assert.equal(
      retained.prepare("SELECT COUNT(*) AS count FROM revisions").get()![
        "count"
      ],
      1,
    );
  } finally {
    retained.close();
  }
});

test("format 1 is rejected without migration or changes to its bytes", (t) => {
  const { store, writeText, directory } = fixture(t);
  writeText("retained");
  store.close();
  const file = join(directory, "hive.sqlite"),
    old = new DatabaseSync(file);
  try {
    old.exec("PRAGMA user_version=1");
  } finally {
    old.close();
  }
  const before = digest(readFileSync(file));
  assert.throws(
    () => new HiveStore(file),
    hasCode("unsupported_storage_format"),
  );
  assert.throws(
    () => inspectStoppedStorage(file, []),
    hasCode("unsupported_storage_format"),
  );
  assert.equal(digest(readFileSync(file)), before);
});

test("search identity survives VACUUM, updates and renames without stale or duplicate hits", (t) => {
  const { store, objects, writeText } = fixture(t);
  const first = writeText("originaltitle", "firstneedle"),
    second = writeText("secondtitle", "secondneedle");
  const identities = () =>
    store.all("SELECT id,search_id FROM objects ORDER BY id");
  const original = identities();
  store.db.exec("VACUUM");
  assert.deepEqual(identities(), original);
  objects.write(context, {
    mutationId: "search-update",
    objectId: first.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: {},
    content: { encoding: "text", value: "replacementneedle" },
  });
  objects.move(context, {
    mutationId: "search-rename",
    objectId: first.object.id,
    parentId: second.object.id,
    name: "replacementtitle",
  });
  const ids = (text: string) =>
    objects.search({ text }).items.map((item) => item.id);
  assert.deepEqual(ids("firstneedle"), []);
  assert.deepEqual(ids("originaltitle"), []);
  assert.deepEqual(ids("replacementneedle"), [first.object.id]);
  assert.deepEqual(ids("replacementtitle"), [first.object.id]);
  assert.deepEqual(ids("secondneedle"), [second.object.id]);
  assert.deepEqual(identities(), original);
  assert.equal(store.get("SELECT count(*) AS n FROM object_fts")!["n"], 2);
  assert.equal(
    store.get(
      "SELECT count(*) AS n FROM object_fts f JOIN objects o ON f.rowid=o.search_id AND f.object_id=o.id",
    )!["n"],
    2,
  );
});

test("exact versions, CAS, immutable history and stable principal-wide replay", (t) => {
  const { store, objects, writeText } = fixture(t);
  const first = writeText("page");
  objects.registerAgent(context, {
    definition: { ...textContract, version: "2.0.0" },
    mutationId: "v2",
  });
  const append: ObjectWrite = {
    mutationId: "append",
    objectId: first.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: {},
    content: { encoding: "text", value: "old-contract-still-valid" },
  };
  const second = objects.write(context, append);
  objects.write(context, {
    ...append,
    mutationId: "upgrade",
    expectedRevision: 2,
    contractVersion: "2.0.0",
  });
  assert.deepEqual(objects.write(context, append), second);
  assert.throws(
    () =>
      objects.write(context, {
        ...append,
        content: { encoding: "text", value: "different" },
      }),
    hasCode("mutation_conflict"),
  );
  assert.throws(
    () =>
      objects.archive(context, {
        objectId: first.object.id,
        archived: true,
        mutationId: "append",
      }),
    hasCode("mutation_conflict"),
  );
  assert.throws(
    () => objects.write(context, { ...append, mutationId: "stale" }),
    hasCode("revision_conflict"),
  );
  assert.throws(
    () =>
      objects.write(context, {
        ...append,
        mutationId: "downgrade",
        expectedRevision: 3,
      }),
    hasCode("contract_version_conflict"),
  );
  const old = objects.read({ objectId: first.object.id, revision: 1 });
  assert.equal(old.object.currentRevision, 3);
  assert.equal(old.revision.contractVersion, "1.0.0");
  assert.deepEqual(old.content, { encoding: "text", value: "hello" });
  const rotated = {
    principalId: credential.principalId,
    digest: digest("replacement-synthetic"),
  };
  store.configureCredentials([rotated]);
  assert.throws(() => store.authenticate(context), hasCode("unauthenticated"));
  assert.deepEqual(
    objects.write(
      { credentialDigest: rotated.digest, principalId: rotated.principalId },
      append,
    ),
    second,
  );
});

test("identical content with a new mutation identity creates a separate revision while original retries remain deduplicated", (t) => {
  const { store, objects, writeText, oid } = fixture(t);
  const first = writeText("same-content", "unchanged text");
  const original: ObjectWrite = {
    mutationId: "same-content-a",
    objectId: first.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: {},
    content: { encoding: "text", value: "unchanged text" },
  };
  const second = objects.write(context, original);
  assert.equal(second.revision.revision, 2);
  assert.equal(second.revision.contentHash, first.revision.contentHash);
  assert.deepEqual(objects.write(context, original), second);
  assert.throws(
    () => objects.write(context, { ...original, mutationId: "same-content-b" }),
    hasCode("revision_conflict"),
  );
  assert.equal(
    store.get(
      "SELECT count(*) AS n FROM mutations WHERE mutation_id=?",
      oid("same-content-b"),
    )!["n"],
    0,
  );
  const independent = {
    ...original,
    mutationId: "same-content-b",
    expectedRevision: 2,
  };
  const third = objects.write(context, independent);
  assert.equal(third.revision.revision, 3);
  assert.equal(third.revision.contentHash, first.revision.contentHash);
  assert.deepEqual(objects.write(context, original), second);
  assert.deepEqual(objects.write(context, independent), third);
  assert.equal(objects.stat({ objectId: first.object.id }).currentRevision, 3);
  assert.equal(
    store.get(
      "SELECT count(*) AS n FROM revisions WHERE object_id=?",
      first.object.id,
    )!["n"],
    3,
  );
  assert.equal(store.get("SELECT count(*) AS n FROM events")!["n"], 3);
  assert.equal(
    store.get(
      "SELECT count(*) AS n FROM mutations WHERE mutation_id IN (?,?)",
      oid("same-content-a"),
      oid("same-content-b"),
    )!["n"],
    2,
  );
});

test("mutation receipt capacity preserves replay, refuses effects atomically, reclaims expiry and survives restart", (t) => {
  const { store, directory } = fixture(t);
  const now = Date.now();
  const mutationId = (name: string, issuedAt = now) =>
    operationId(store.runtimeEpoch, issuedAt, digest(name).slice(7));
  const originalId = mutationId("receipt-capacity-original");
  const original = store.mutate(
    context,
    "test.receiptCapacity",
    { mutationId: originalId, value: 1 },
    () => ({ value: { accepted: "original" } }),
  );
  const existing = Number(
    store.get("SELECT COUNT(*) AS count FROM mutations")!["count"],
  );
  const remaining = mutationReceiptCount - existing;
  store.run(
    `WITH RECURSIVE sequence(value) AS (
      SELECT 0 UNION ALL SELECT value+1 FROM sequence WHERE value+1<?
    ) INSERT INTO mutations
      SELECT ?, 'capacity-'||value, ?, ?, ?, 'snapshot', '{}', 128 FROM sequence`,
    remaining,
    credential.principalId,
    digest("capacity-request"),
    now,
    now,
  );
  store.setMetadata(
    "mutation_receipt_bytes",
    String(
      store.get("SELECT SUM(receipt_bytes) AS bytes FROM mutations")!["bytes"],
    ),
  );
  assert.equal(
    store.get("SELECT COUNT(*) AS count FROM mutations")!["count"],
    mutationReceiptCount,
  );
  let effects = 0;
  assert.deepEqual(
    store.mutate(
      context,
      "test.receiptCapacity",
      { mutationId: originalId, value: 1 },
      () => {
        effects++;
        return { value: { accepted: "replacement" } };
      },
    ),
    original,
  );
  const refusedId = mutationId("receipt-capacity-refused");
  assert.throws(
    () =>
      store.mutate(
        context,
        "test.receiptCapacity",
        { mutationId: refusedId },
        () => {
          effects++;
          return { value: { accepted: "refused" } };
        },
      ),
    hasCode("capacity_exceeded"),
  );
  assert.equal(effects, 0);
  assert.equal(
    store.get(
      "SELECT COUNT(*) AS count FROM mutations WHERE mutation_id=?",
      refusedId,
    )!["count"],
    0,
  );

  store.run(
    "UPDATE mutations SET issued_at_ms=?,completed_at_ms=? WHERE mutation_id='capacity-0'",
    now - 24 * 60 * 60 * 1000 - 1,
    now - 24 * 60 * 60 * 1000 - 1,
  );
  const cutoff = now - 24 * 60 * 60 * 1000;
  store.run("UPDATE mutations SET issued_at_ms=?,completed_at_ms=? WHERE mutation_id='capacity-1'", cutoff, now);
  store.run("UPDATE mutations SET issued_at_ms=?,completed_at_ms=? WHERE mutation_id='capacity-2'", now, cutoff);
  const reclaimedId = mutationId("receipt-capacity-reclaimed", now + 1);
  const reclaimed = store.mutate(
    context,
    "test.receiptCapacity",
    { mutationId: reclaimedId },
    () => ({ value: { accepted: "reclaimed" } }),
  );
  assert.deepEqual(reclaimed, { accepted: "reclaimed" });
  assert.equal(store.get("SELECT COUNT(*) AS count FROM mutations WHERE mutation_id IN ('capacity-1','capacity-2')")!["count"], 2);
  assert.equal(
    store.get("SELECT COUNT(*) AS count FROM mutations")!["count"],
    mutationReceiptCount,
  );

  const filename = join(directory, "hive.sqlite");
  store.close();
  const restarted = new HiveStore(filename);
  restarted.configureCredentials([credential]);
  try {
    assert.deepEqual(
      restarted.mutate(
        context,
        "test.receiptCapacity",
        { mutationId: reclaimedId },
        () => {
          effects++;
          return { value: { accepted: "replacement" } };
        },
      ),
      reclaimed,
    );
    assert.equal(effects, 0);
  } finally {
    restarted.close();
  }
});

test("mutation receipt byte capacity reserves the maximum result before applying work", (t) => {
  const { store } = fixture(t);
  const now = Date.now();
  store.setMetadata(
    "mutation_receipt_bytes",
    String(mutationReceiptJournalBytes - mutationReceiptResultBytes),
  );
  assert.deepEqual(
    store.mutate(
      context,
      "test.receiptBytes",
      {
        mutationId: operationId(
          store.runtimeEpoch,
          now,
          digest("receipt-byte-boundary").slice(7),
        ),
      },
      () => ({ value: { accepted: "boundary" } }),
    ),
    { accepted: "boundary" },
  );
  store.setMetadata(
    "mutation_receipt_bytes",
    String(mutationReceiptJournalBytes - mutationReceiptResultBytes + 1),
  );
  let applied = false;
  assert.throws(
    () =>
      store.mutate(
        context,
        "test.receiptBytes",
        {
          mutationId: operationId(
            store.runtimeEpoch,
            now,
            digest("receipt-byte-refusal").slice(7),
          ),
        },
        () => {
          applied = true;
          return { value: { accepted: true } };
        },
      ),
    hasCode("capacity_exceeded"),
  );
  assert.equal(applied, false);
});

test("database failure rolls back content, current pointer, FTS, event and mutation ledger", (t) => {
  const { store, objects, writeText, oid } = fixture(t);
  const first = writeText("page", "original text");
  store.db.exec(
    "CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected_storage_failure'); END",
  );
  const append: ObjectWrite = {
    mutationId: "failed-write",
    objectId: first.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: {},
    content: { encoding: "text", value: "changed unique word" },
  };
  assert.throws(() => objects.write(context, append));
  assert.equal(objects.stat({ objectId: first.object.id }).currentRevision, 1);
  assert.equal(objects.search({ text: "changed" }).items.length, 0);
  assert.equal(objects.search({ text: "original" }).items.length, 1);
  assert.equal(store.get("SELECT count(*) AS n FROM events")!["n"], 1);
  assert.equal(store.get("SELECT count(*) AS n FROM revisions")!["n"], 1);
  assert.equal(
    store.get(
      "SELECT count(*) AS n FROM mutations WHERE mutation_id=?",
      oid("failed-write"),
    )!["n"],
    0,
  );
  store.db.exec("DROP TRIGGER fail_event");
  assert.equal(objects.write(context, append).revision.revision, 2);
});

test("actual SQLITE_FULL during a large write leaves no partial Object or mutation", (t) => {
  const { store, objects, oid } = fixture(t);
  store.db.exec("PRAGMA wal_checkpoint(TRUNCATE)");
  const pages = Number(store.get("PRAGMA page_count")!["page_count"]);
  store.db.exec(`PRAGMA max_page_count=${pages + 1}`);
  assert.throws(
    () =>
      objects.write(context, {
        mutationId: "disk-full",
        contractVersion: "1.0.0",
        references: {},
        create: {
          contractKey: textContract.key,
          parentId: null,
          ownerObjectId: null,
          name: "too-large-for-disk",
        },
        content: { encoding: "text", value: "x".repeat(512 * 1024) },
      }),
    /full/i,
  );
  assert.equal(store.get("SELECT count(*) AS n FROM objects")!["n"], 0);
  assert.equal(store.get("SELECT count(*) AS n FROM events")!["n"], 0);
  assert.equal(
    store.get(
      "SELECT count(*) AS n FROM mutations WHERE mutation_id=?",
      oid("disk-full"),
    )!["n"],
    0,
  );
});

test("BLOB storage is decoded and payload is absent from event and ledger", (t) => {
  const { store, objects, oid } = fixture(t);
  const binary: DataContract = {
    ...textContract,
    key: "test/binary",
    mediaType: "application/octet-stream",
  };
  objects.registerAgent(context, {
    definition: binary,
    mutationId: "binary-contract",
  });
  const value = Buffer.alloc(100_000, 0xde).toString("base64");
  const saved = objects.write(context, {
    mutationId: "binary",
    contractVersion: "1.0.0",
    references: {},
    create: {
      contractKey: binary.key,
      name: "attachment",
      parentId: null,
      ownerObjectId: null,
    },
    content: { encoding: "base64", value },
  });
  assert.equal(
    store.get("SELECT typeof(content) AS kind FROM revisions")!["kind"],
    "blob",
  );
  assert.deepEqual(objects.read({ objectId: saved.object.id }).content, {
    encoding: "base64",
    value,
  });
  assert.ok(
    Number(
      store.get(
        "SELECT length(result_json) AS size FROM mutations WHERE mutation_id=?",
        oid("binary"),
      )!["size"],
    ) < 2048,
  );
  assert.ok(
    Number(store.get("SELECT length(payload) AS size FROM events")!["size"]) <
      256,
  );
  assert.throws(
    () =>
      objects.write(context, {
        mutationId: "invalid-base64",
        contractVersion: "1.0.0",
        references: {},
        create: {
          contractKey: binary.key,
          name: "bad",
          parentId: null,
          ownerObjectId: null,
        },
        content: { encoding: "base64", value: "Zh==" },
      }),
    hasCode("invalid_arguments"),
  );
});

test("normalized sibling uniqueness, cycles, inherited archive and derived paths", (t) => {
  const { objects, writeText } = fixture(t);
  const root = writeText("café");
  assert.throws(() => writeText("cafe\u0301"), hasCode("revision_conflict"));
  const child = writeText("child", "needle", root.object.id);
  const grandchild = writeText("nested", "needle", child.object.id);
  assert.throws(
    () =>
      objects.move(context, {
        objectId: root.object.id,
        parentId: child.object.id,
        name: "cycle",
        mutationId: "cycle",
      }),
    hasCode("invalid_arguments"),
  );
  objects.archive(context, {
    objectId: root.object.id,
    archived: true,
    mutationId: "archive-root",
  });
  assert.equal(objects.list({ parentId: null }).items.length, 0);
  assert.equal(
    objects.stat({ objectId: grandchild.object.id }).effectivelyArchived,
    true,
  );
  assert.equal(objects.search({ text: "needle" }).items.length, 0);
  const update = { objectId: grandchild.object.id, expectedRevision: 1, contractVersion: grandchild.revision.contractVersion,
    content: { encoding: "text" as const, value: "needle" }, references: {} };
  assert.throws(() => objects.write(context, { ...update, expectedArchived: false, mutationId: "active-only-write" }),
    hasCode("revision_conflict"));
  const archivedWrite = objects.write(context, { ...update, expectedArchived: true, mutationId: "archived-write" });
  assert.equal(archivedWrite.object.currentRevision, 2);
  assert.throws(() => objects.archive(context, { objectId: grandchild.object.id, expectedRevision: 1,
    archived: false, mutationId: "stale-unarchive" }), hasCode("revision_conflict"));
  objects.archive(context, {
    objectId: child.object.id,
    archived: true,
    mutationId: "archive-child",
  });
  objects.archive(context, {
    objectId: root.object.id,
    archived: false,
    mutationId: "unarchive-root",
  });
  assert.equal(
    objects.stat({ objectId: child.object.id }).effectivelyArchived,
    true,
  );
  objects.move(context, {
    objectId: child.object.id,
    parentId: null,
    name: "moved",
    mutationId: "move-child",
  });
  assert.equal(
    objects.stat({ objectId: grandchild.object.id }).path,
    "/moved/nested",
  );
  assert.equal(
    objects.search({
      text: "needle",
      rootId: root.object.id,
      includeArchived: true,
    }).items.length,
    0,
  );
  assert.equal(
    objects.read({ objectId: grandchild.object.id, revision: 1 }).object.path,
    "/moved/nested",
  );
  assert.throws(() => writeText("../bad"), hasCode("invalid_arguments"));
});

test("object icons and dense sibling positions support deterministic reorder and append", (t) => {
  const { objects, store, writeText } = fixture(t);
  const parent = writeText("parent"),
    other = writeText("other");
  const alpha = writeText("alpha", "", parent.object.id),
    beta = writeText("beta", "", parent.object.id),
    gamma = writeText("gamma", "", parent.object.id);
  assert.deepEqual(
    [alpha, beta, gamma].map((item) => [
      item.object.position,
      item.object.icon,
    ]),
    [
      [0, null],
      [1, null],
      [2, null],
    ],
  );

  const decorated = objects.move(context, {
    objectId: beta.object.id,
    parentId: parent.object.id,
    name: "beta",
    icon: "📘",
    mutationId: "decorate-beta",
  });
  assert.equal(decorated.icon, "📘");
  assert.equal(decorated.position, 1);
  assert.throws(
    () =>
      objects.move(context, {
        objectId: beta.object.id,
        parentId: parent.object.id,
        name: "beta",
        icon: "\n",
        mutationId: "invalid-icon",
      }),
    hasCode("invalid_arguments"),
  );

  const reordered = objects.reorder(context, {
    objectId: gamma.object.id,
    beforeObjectId: alpha.object.id,
    mutationId: "gamma-first",
  });
  assert.equal(reordered.position, 0);
  assert.deepEqual(
    queryObjects(store, {
      contractKey: textContract.key,
      where: { op: "eq", field: "object.parentId", value: parent.object.id },
      select: ["object.name", "object.position", "object.icon"],
      orderBy: [{ field: "object.position", direction: "asc" }],
    }).items.map((item) => [
      item.values["object.name"],
      item.values["object.position"],
      item.values["object.icon"],
    ]),
    [
      ["gamma", 0, null],
      ["alpha", 1, null],
      ["beta", 2, "📘"],
    ],
  );
  const firstPage = objects.list({ parentId: parent.object.id, limit: 2 });
  assert.deepEqual(
    [
      ...firstPage.items,
      ...objects.list({
        parentId: parent.object.id,
        limit: 2,
        cursor: firstPage.nextCursor!,
      }).items,
    ].map((item) => item.name),
    ["gamma", "alpha", "beta"],
    "listing pages follow sibling navigation order",
  );
  assert.deepEqual(
    queryObjects(store, {
      contractKey: textContract.key,
      where: { op: "isNull", field: "object.parentId" },
      select: ["object.name", "object.hasContractChildren"],
      orderBy: [{ field: "object.position", direction: "asc" }],
    }).items.map((item) => [
      item.values["object.name"],
      item.values["object.hasContractChildren"],
    ]),
    [
      ["parent", true],
      ["other", false],
    ],
    "navigation can hide empty branches without loading them",
  );
  assert.deepEqual(
    objects.reorder(context, {
      objectId: gamma.object.id,
      beforeObjectId: alpha.object.id,
      mutationId: "gamma-first",
    }),
    reordered,
  );
  assert.throws(
    () =>
      objects.reorder(context, {
        objectId: gamma.object.id,
        beforeObjectId: other.object.id,
        mutationId: "cross-parent-order",
      }),
    hasCode("invalid_arguments"),
  );

  objects.move(context, {
    objectId: alpha.object.id,
    parentId: other.object.id,
    name: "alpha",
    mutationId: "move-alpha",
  });
  assert.deepEqual(
    store
      .all(
        "SELECT name,position FROM objects WHERE parent_id=? ORDER BY position",
        parent.object.id,
      )
      .map((row) => [row["name"], row["position"]]),
    [
      ["gamma", 0],
      ["beta", 1],
    ],
  );
  assert.equal(writeText("delta", "", parent.object.id).object.position, 2);
});

test("subtree navigation is one atomic set update across wide, deep and explicitly archived branches", (t) => {
  const { store, objects, writeText, oid } = fixture(t),
    archivedParent = writeText("archived-parent"),
    root = writeText("set-root");
  const descendants: string[] = [];
  for (let branch = 0; branch < 12; branch++) {
    let parent = writeText(
      "branch-" + branch,
      "searchable branch",
      root.object.id,
    ).object.id;
    descendants.push(parent);
    for (let depth = 0; depth < 4; depth++) {
      parent = writeText(
        `level-${branch}-${depth}`,
        "searchable descendant",
        parent,
      ).object.id;
      descendants.push(parent);
    }
  }
  const explicit = descendants[12]!,
    explicitChild = descendants[13]!;
  objects.archive(context, {
    objectId: archivedParent.object.id,
    archived: true,
    mutationId: "archive-target",
  });
  objects.archive(context, {
    objectId: explicit,
    archived: true,
    mutationId: "archive-explicit",
  });
  const original = {
    all: store.all.bind(store),
    get: store.get.bind(store),
    run: store.run.bind(store),
  };
  let calls = 0,
    navigationStatements = 0;
  store.all = ((...args: Parameters<HiveStore["all"]>) => {
    calls++;
    return original.all(...args);
  }) as HiveStore["all"];
  store.get = ((...args: Parameters<HiveStore["get"]>) => {
    calls++;
    return original.get(...args);
  }) as HiveStore["get"];
  store.run = ((...args: Parameters<HiveStore["run"]>) => {
    calls++;
    if (String(args[0]).includes("WITH RECURSIVE navigation"))
      navigationStatements++;
    return original.run(...args);
  }) as HiveStore["run"];
  let moved;
  try {
    moved = objects.move(context, {
      objectId: root.object.id,
      parentId: archivedParent.object.id,
      name: "inside-archive",
      mutationId: "wide-move-in",
    });
  } finally {
    store.all = original.all;
    store.get = original.get;
    store.run = original.run;
  }
  assert.equal(navigationStatements, 1);
  assert.ok(
    calls < descendants.length,
    `set navigation used ${calls} SQL calls for ${descendants.length + 1} Objects`,
  );
  assert.equal(moved.path, "/archived-parent/inside-archive");
  assert.equal(moved.currentRevision, 1);
  assert.ok(
    descendants.every(
      (id) => objects.stat({ objectId: id }).effectivelyArchived,
    ),
  );
  const outside = objects.move(context, {
    objectId: root.object.id,
    parentId: null,
    name: "outside-archive",
    mutationId: "wide-move-out",
  });
  assert.equal(outside.effectivelyArchived, false);
  assert.equal(objects.stat({ objectId: explicit }).effectivelyArchived, true);
  assert.equal(
    objects.stat({ objectId: explicitChild }).effectivelyArchived,
    true,
  );
  assert.equal(
    objects.stat({ objectId: descendants[0]! }).effectivelyArchived,
    false,
  );
  assert.equal(
    objects.search({ text: "searchable", rootId: root.object.id, limit: 100 })
      .items.length,
    descendants.length - 3,
  );
  const eventsAfterMove = Number(
    store.get("SELECT COUNT(*) AS count FROM events")!["count"],
  );
  assert.deepEqual(
    objects.move(context, {
      objectId: root.object.id,
      parentId: null,
      name: "outside-archive",
      mutationId: "wide-move-out",
    }),
    outside,
  );
  assert.equal(
    Number(store.get("SELECT COUNT(*) AS count FROM events")!["count"]),
    eventsAfterMove,
  );

  const failingRoot = writeText("failure-root", "failure root"),
    failingChild = writeText(
      "failure-child",
      "failure child",
      failingRoot.object.id,
    );
  const before = store.all(
    "SELECT id,parent_id,name,path,depth,archived_at,effective_archive,current_revision FROM objects WHERE id IN (?,?) ORDER BY id",
    failingRoot.object.id,
    failingChild.object.id,
  );
  const events = Number(
    store.get("SELECT COUNT(*) AS count FROM events")!["count"],
  );
  store.db.exec(
    `CREATE TRIGGER fail_navigation BEFORE UPDATE OF path ON objects WHEN NEW.id='${failingChild.object.id}' BEGIN SELECT RAISE(ABORT,'forced_navigation_failure'); END;`,
  );
  assert.throws(
    () =>
      objects.move(context, {
        objectId: failingRoot.object.id,
        parentId: root.object.id,
        name: "failed-rename",
        mutationId: "forced-navigation-failure",
      }),
    /forced_navigation_failure/,
  );
  store.db.exec("DROP TRIGGER fail_navigation");
  assert.deepEqual(
    store.all(
      "SELECT id,parent_id,name,path,depth,archived_at,effective_archive,current_revision FROM objects WHERE id IN (?,?) ORDER BY id",
      failingRoot.object.id,
      failingChild.object.id,
    ),
    before,
  );
  assert.equal(
    Number(store.get("SELECT COUNT(*) AS count FROM events")!["count"]),
    events,
  );
  assert.equal(
    Number(
      store.get(
        "SELECT COUNT(*) AS count FROM mutations WHERE mutation_id=?",
        oid("forced-navigation-failure"),
      )!["count"],
    ),
    0,
  );
  assert.deepEqual(
    objects
      .search({ text: "failure" })
      .items.map((item) => item.name)
      .sort(),
    ["failure-child", "failure-root"],
  );
  assert.equal(objects.search({ text: "failed-rename" }).items.length, 0);
});

test("typed predicates, JSON pointer keys, null semantics and version admission", (t) => {
  const { objects, store } = fixture(t);
  const contract: DataContract = {
    ...textContract,
    key: "test/query",
    mediaType: "application/json",
    jsonSchema: {
      type: "object",
      properties: {
        value: { type: ["boolean", "number", "string", "null"] },
        "a/b": { type: "integer" },
      },
      additionalProperties: false,
    },
  };
  objects.registerAgent(context, {
    definition: contract,
    mutationId: "query-contract",
  });
  for (const [index, value] of [
    null,
    false,
    0,
    true,
    1,
    1.5,
    "0",
    "1",
    "z",
    undefined,
  ].entries()) {
    objects.write(context, {
      mutationId: "q" + index,
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: contract.key,
        name: "q" + index,
        parentId: null,
        ownerObjectId: null,
      },
      content: {
        encoding: "json",
        value: { ...(value === undefined ? {} : { value }), "a/b": index },
      },
    });
  }
  const query = (where: Parameters<typeof queryObjects>[1]["where"]) =>
    queryObjects(store, {
      contractKey: contract.key,
      ...(where ? { where } : {}),
    });
  assert.equal(
    query({ op: "eq", field: "data:/value", value: false }).items.length,
    1,
  );
  assert.equal(
    query({ op: "eq", field: "data:/value", value: 0 }).items.length,
    1,
  );
  assert.equal(query({ op: "isNull", field: "data:/value" }).items.length, 2);
  assert.equal(
    query({ op: "eq", field: "data:/value", value: null }).items.length,
    0,
  );
  assert.equal(
    query({ op: "ne", field: "data:/value", value: false }).items.length,
    1,
  );
  assert.equal(
    query({ op: "not", arg: { op: "eq", field: "data:/value", value: false } })
      .items.length,
    9,
  );
  assert.equal(
    query({ op: "gte", field: "data:/a~1b", value: 8 }).items.length,
    2,
  );
  assert.throws(
    () =>
      queryObjects(store, {
        contractKey: contract.key,
        contractVersions: ["1.0.0"],
        where: { op: "eq", field: "data:/a~1b", value: 1.5 },
      }),
    hasCode("invalid_arguments"),
  );
});

test("ownerObjectId supports projection, nullable predicates, ordering and stable paging", (t) => {
  const { objects, store, writeText } = fixture(t);
  const firstOwner = writeText("first-owner").object.id,
    secondOwner = writeText("second-owner").object.id;
  const parent = writeText("navigation-parent").object.id;
  const contract: DataContract = {
    ...textContract,
    key: "test/owned-query",
    retention: {
      objects: { mode: "owned" },
      revisions: { mode: "current" },
    },
  };
  objects.registerAgent(context, {
    definition: contract,
    mutationId: "owned-query-contract",
  });
  const create = (mutationId: string, name: string, ownerObjectId: string) =>
    objects.write(context, {
      mutationId,
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: contract.key,
        name,
        parentId: null,
        ownerObjectId,
      },
      content: { encoding: "text", value: name },
    });
  const rows = [
    create("owned-a", "a", firstOwner),
    create("owned-b", "b", firstOwner),
    create("owned-c", "c", secondOwner),
  ];
  const request = {
    contractKey: contract.key,
    select: ["object.ownerObjectId"],
    where: {
      op: "eq" as const,
      field: "object.ownerObjectId",
      value: firstOwner,
    },
    orderBy: [{ field: "object.ownerObjectId", direction: "asc" as const }],
    limit: 1,
  };
  const first = queryObjects(store, request),
    second = queryObjects(store, { ...request, cursor: first.nextCursor! });
  assert.deepEqual(
    [...first.items, ...second.items].map((item) => item.objectId),
    rows
      .slice(0, 2)
      .map((row) => row.object.id)
      .sort(),
  );
  assert.ok(first.nextCursor);
  assert.equal(second.nextCursor, null);
  assert.equal(
    queryObjects(store, {
      ...request,
      where: {
        op: "in",
        field: "object.ownerObjectId",
        value: [secondOwner, null],
      },
      limit: 10,
    }).items[0]?.objectId,
    rows[2]!.object.id,
  );
  assert.equal(
    queryObjects(store, {
      contractKey: textContract.key,
      where: { op: "isNull", field: "object.ownerObjectId" },
    }).items.length,
    3,
  );
  assert.throws(
    () =>
      queryObjects(store, {
        ...request,
        where: { op: "eq", field: "object.ownerObjectId", value: 1 },
      }),
    hasCode("invalid_arguments"),
  );
  objects.move(context, {
    objectId: rows[0]!.object.id,
    parentId: parent,
    name: "moved-owned",
    mutationId: "move-owned",
  });
  assert.equal(
    queryObjects(store, { ...request, limit: 10 }).items.find(
      (item) => item.objectId === rows[0]!.object.id,
    )?.values["object.ownerObjectId"],
    firstOwner,
  );
});

test("isNull distinguishes missing and null data from present objects and arrays across paging and updates", (t) => {
  const { objects, store } = fixture(t);
  const contract: DataContract = {
    ...textContract,
    key: "test/nullable-decision",
    mediaType: "application/json",
    jsonSchema: {
      type: "object",
      properties: {
        decision: {
          anyOf: [
            { type: "null" },
            { type: "object" },
            { type: "array" },
            { type: "string" },
            { type: "number" },
            { type: "boolean" },
          ],
        },
      },
      additionalProperties: false,
    },
  };
  objects.registerAgent(context, {
    definition: contract,
    mutationId: "nullable-decision-contract",
  });
  const values: (Json | undefined)[] = [
    undefined,
    {},
    null,
    { summary: "Saved assessment" },
    [],
    [{}],
    "",
    0,
    false,
  ];
  const rows = values.map((decision, i) =>
    objects.write(context, {
      mutationId: "nullable-decision-" + i,
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: contract.key,
        name: String(i),
        parentId: null,
        ownerObjectId: null,
      },
      content: {
        encoding: "json",
        value: decision === undefined ? {} : { decision },
      },
    }),
  );
  const where = { op: "isNull" as const, field: "data:/decision" };
  for (const contractVersions of [undefined, ["1.0.0"]]) {
    const request = {
      contractKey: contract.key,
      ...(contractVersions ? { contractVersions } : {}),
      where,
      orderBy: [{ field: "object.name", direction: "asc" as const }],
      limit: 1,
    };
    const first = queryObjects(store, request);
    assert.equal(first.items[0]?.objectId, rows[0]!.object.id);
    assert.ok(first.nextCursor);
    const second = queryObjects(store, {
      ...request,
      cursor: first.nextCursor!,
    });
    assert.equal(second.items[0]?.objectId, rows[2]!.object.id);
    assert.equal(second.nextCursor, null);
    const present = queryObjects(store, {
      ...request,
      where: { op: "not", arg: where },
      limit: 20,
    });
    assert.deepEqual(
      present.items.map((row) => row.objectId),
      rows.filter((_row, i) => i !== 0 && i !== 2).map((row) => row.object.id),
    );
  }
  objects.write(context, {
    mutationId: "assessment-saved",
    objectId: rows[2]!.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: {},
    content: {
      encoding: "json",
      value: { decision: { summary: "The message is now assessed." } },
    },
  });
  assert.deepEqual(
    queryObjects(store, { contractKey: contract.key, where }).items.map(
      (row) => row.objectId,
    ),
    [rows[0]!.object.id],
  );
  assert.equal(
    queryObjects(store, {
      contractKey: contract.key,
      where: {
        op: "and",
        args: [
          { op: "eq", field: "object.id", value: rows[1]!.object.id },
          { op: "isNull", field: "data:/decision/summary" },
        ],
      },
    }).items.length,
    1,
    "A genuinely missing child of a present object still matches isNull.",
  );
});

test("scalar enum and const fields can be selected and filtered without admitting structured enums", (t) => {
  const { objects, store } = fixture(t);
  const contract: DataContract = {
    ...textContract,
    key: "test/literal-scalars",
    mediaType: "application/json",
    jsonSchema: {
      type: "object",
      properties: {
        kind: { enum: ["final", "intermediate"] },
        fixed: { const: "reviewed" },
        mixed: { enum: [null, false, 2, "two"] },
        structured: { enum: ["text", { name: "nested" }] },
        nested: { const: { name: "fixed" } },
      },
      additionalProperties: false,
    },
  };
  objects.registerAgent(context, {
    definition: contract,
    mutationId: "literal-scalars",
  });
  objects.write(context, {
    mutationId: "literal-row",
    contractVersion: "1.0.0",
    references: {},
    create: {
      contractKey: contract.key,
      name: "row",
      parentId: null,
      ownerObjectId: null,
    },
    content: {
      encoding: "json",
      value: {
        kind: "final",
        fixed: "reviewed",
        mixed: false,
        structured: { name: "nested" },
        nested: { name: "fixed" },
      },
    },
  });
  const request = {
    contractKey: contract.key,
    contractVersions: ["1.0.0"],
    select: ["data:/kind", "data:/fixed", "data:/mixed"],
  };
  const result = queryObjects(store, {
    ...request,
    where: {
      op: "and",
      args: [
        { op: "eq", field: "data:/kind", value: "final" },
        { op: "contains", field: "data:/fixed", value: "view" },
      ],
    },
  });
  assert.equal(result.items.length, 1);
  assert.deepEqual(
    { ...result.items[0]!.values },
    { "data:/kind": "final", "data:/fixed": "reviewed", "data:/mixed": false },
  );
  for (const field of ["data:/structured", "data:/nested"])
    assert.throws(
      () => queryObjects(store, { ...request, select: [field] }),
      hasCode("invalid_arguments"),
    );
  assert.throws(
    () =>
      queryObjects(store, {
        ...request,
        where: { op: "eq", field: "data:/kind", value: 1 },
      }),
    hasCode("invalid_arguments"),
  );
});

test("literal substring search composes with root and state before pagination and never coerces other types", (t) => {
  const { objects, store, writeText } = fixture(t);
  const root = writeText("scope").object.id;
  const needle = "%_'\\ 日本語 🧪";
  const contract: DataContract = {
    ...textContract,
    key: "test/substring",
    mediaType: "application/json",
    jsonSchema: {
      type: "object",
      properties: {
        title: { type: "string" },
        description: { type: ["string", "null"] },
        status: { enum: ["ready", "completed"] },
        value: {},
        count: { type: "integer" },
      },
      additionalProperties: false,
    },
  };
  objects.registerAgent(context, {
    definition: contract,
    mutationId: "substring-contract",
  });
  for (let index = 0; index < 8; index++)
    objects.write(context, {
      mutationId: "substring-" + index,
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: contract.key,
        name: "opaque-" + index,
        parentId: index === 6 ? null : root,
        ownerObjectId: null,
      },
      content: {
        encoding: "json",
        value: {
          title: index % 2 ? "plain" : "Title " + needle,
          description: index % 2 ? "Description " + needle : null,
          status: index === 7 ? "completed" : "ready",
          count: index,
          ...(index === 7
            ? {}
            : {
                value: [null, false, 0, { x: needle }, [needle], "", needle][
                  index
                ]!,
              }),
        },
      },
    });
  const search = {
    op: "or" as const,
    args: ["title", "description"].map((field) => ({
      op: "contains" as const,
      field: "data:/" + field,
      value: needle,
    })),
  };
  const request = {
    contractKey: contract.key,
    contractVersions: ["1.0.0"],
    where: {
      op: "and" as const,
      args: [
        { op: "eq" as const, field: "object.parentId", value: root },
        { op: "eq" as const, field: "data:/status", value: "ready" },
        search,
      ],
    },
    select: ["data:/count"],
    orderBy: [{ field: "data:/count", direction: "asc" as const }],
    limit: 2,
  };
  let page = queryObjects(store, request);
  const first = page.nextCursor;
  const counts = page.items.map((item) => item.values["data:/count"]);
  while (page.nextCursor) {
    page = queryObjects(store, { ...request, cursor: page.nextCursor });
    counts.push(...page.items.map((item) => item.values["data:/count"]));
  }
  assert.deepEqual(counts, [0, 1, 2, 3, 4, 5]);
  assert.ok(first);
  assert.throws(
    () =>
      queryObjects(store, {
        ...request,
        cursor: first,
        where: { op: "contains", field: "data:/title", value: "changed" },
      }),
    hasCode("invalid_cursor"),
  );
  const match = (value: string, field = "data:/value") =>
    queryObjects(store, {
      contractKey: contract.key,
      where: { op: "contains", field, value },
    }).items.length;
  assert.equal(match(needle), 1);
  assert.equal(match(""), 2);
  assert.equal(match("title", "data:/title"), 0);
  assert.equal(match("Title", "data:/title"), 4);
  assert.equal(match("' OR 1=1 --", "data:/title"), 0);
  assert.equal(
    queryObjects(store, {
      contractKey: contract.key,
      where: {
        op: "not",
        arg: { op: "contains", field: "data:/value", value: needle },
      },
    }).items.length,
    7,
  );
  assert.throws(
    () =>
      queryObjects(store, {
        ...request,
        where: { op: "contains", field: "data:/count", value: "1" },
      }),
    hasCode("invalid_arguments"),
  );
  assert.equal(
    objects.search({ text: "日本語", contractKey: contract.key }).items.length,
    0,
    "JSON content is intentionally absent from FTS",
  );
});

test("hierarchy depth is enforced on create and subtree move; tree truncation is explicit", (t) => {
  const { objects, writeText } = fixture(t);
  let parent: string | null = null;
  const chain: string[] = [];
  for (let i = 0; i < 64; i++) {
    parent = writeText("level-" + i, "", parent).object.id;
    chain.push(parent);
  }
  assert.throws(
    () => writeText("overflow", "", parent),
    hasCode("limit_exceeded"),
  );
  const branch = writeText("branch");
  writeText("leaf", "", branch.object.id);
  assert.throws(
    () =>
      objects.move(context, {
        objectId: branch.object.id,
        parentId: chain[62]!,
        name: "too-deep",
        mutationId: "deep-move",
      }),
    hasCode("limit_exceeded"),
  );
  assert.equal(objects.stat({ objectId: branch.object.id }).path, "/branch");
  const tree = objects.tree({ rootId: null, depth: 2, limit: 5 });
  assert.equal(tree.truncated, true);
  assert.ok(tree.items.length <= 5);
});

test("storage allows only one Hive opener, protects domain rows and permits collector event deletion", (t) => {
  const { store, directory, writeText } = fixture(t);
  writeText("immutable");
  assert.throws(() => new HiveStore(join(directory, "hive.sqlite")), /locked/i);
  assert.throws(
    () => store.run("UPDATE revisions SET content=?", "tampered"),
    /immutable/,
  );
  assert.doesNotThrow(() => store.run("DELETE FROM events"));
  assert.throws(
    () => store.run("UPDATE objects SET contract_key=?", "different"),
    /immutable/,
  );
});

test("cursor pages cross mixed types, ties and nulls, and bind every query dimension", (t) => {
  const { store, objects } = fixture(t);
  const contract: DataContract = {
    ...textContract,
    key: "test/pages",
    mediaType: "application/json",
    jsonSchema: {
      type: "object",
      properties: { value: { type: ["null", "boolean", "number", "string"] } },
      additionalProperties: false,
    },
  };
  objects.registerAgent(context, {
    definition: contract,
    mutationId: "pages-contract",
  });
  for (const [index, value] of [
    null,
    false,
    false,
    true,
    -1,
    0,
    2,
    2,
    "a",
    "z",
    null,
    undefined,
  ].entries())
    objects.write(context, {
      mutationId: "row-" + index,
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: contract.key,
        name: "row-" + index,
        parentId: null,
        ownerObjectId: null,
      },
      content: {
        encoding: "json",
        value: value === undefined ? {} : { value },
      },
    });
  const request = {
    contractKey: contract.key,
    orderBy: [{ field: "data:/value", direction: "desc" as const }],
    select: ["data:/value"],
    limit: 2,
  };
  const first = queryObjects(store, request);
  assert.ok(first.nextCursor);
  const values = first.items.map((item) => item.values["data:/value"]);
  const ids = first.items.map((item) => item.objectId);
  let cursor: string | null = first.nextCursor;
  while (cursor) {
    const page = queryObjects(store, { ...request, cursor });
    values.push(...page.items.map((item) => item.values["data:/value"]));
    ids.push(...page.items.map((item) => item.objectId));
    cursor = page.nextCursor;
  }
  assert.deepEqual(values, [
    true,
    false,
    false,
    2,
    2,
    0,
    -1,
    "z",
    "a",
    null,
    null,
    null,
  ]);
  assert.equal(new Set(ids).size, 12);
  for (const changed of [
    { includeArchived: true },
    { select: ["object.name"] },
    { contractVersions: ["1.0.0"] },
    { where: { op: "isNull" as const, field: "data:/value" } },
  ])
    assert.throws(
      () =>
        queryObjects(store, {
          ...request,
          ...changed,
          cursor: first.nextCursor!,
        }),
      hasCode("invalid_cursor"),
    );
  assert.throws(
    () =>
      queryObjects(store, {
        ...request,
        cursor: first.nextCursor!.slice(0, -4) + "evil",
      }),
    hasCode("invalid_cursor"),
  );
});

test("durable pull delivery repeats one outstanding batch across reconnects and keeps exact ack boundaries", (t) => {
  const { store, events, writeText } = fixture(t);
  const nodeId = "test-node";
  store.run(
    "INSERT INTO service_nodes(id,principal_id,host_id,service_name,generation,status_json) VALUES (?,?,?,?,1,?)",
    nodeId,
    credential.principalId,
    "test-host",
    "test-service",
    canonical({ connected: true }),
  );
  const service = { ...context, serviceNodeId: nodeId, generation: 1 };
  for (let i = 0; i < 5; i++) writeText("event-" + i);
  assert.deepEqual(events.head(), { throughSequence: 5 });
  const first = events.subscribe(service, {
    name: "consumer",
    filter: {},
    limit: 2,
  });
  assert.equal(first.items.length, 2);
  assert.deepEqual(
    events.subscribe(service, { name: "consumer", filter: {}, limit: 5 }),
    first,
  );
  assert.throws(
    () => events.ack(service, { name: "consumer", throughSequence: 5 }),
    hasCode("invalid_ack"),
  );
  assert.throws(
    () =>
      events.subscribe(service, {
        name: "consumer",
        filter: { topics: ["other"] },
      }),
    hasCode("subscription_filter_conflict"),
  );
  events.ack(service, {
    name: "consumer",
    throughSequence: first.throughSequence,
  });
  events.ack(service, {
    name: "consumer",
    throughSequence: first.throughSequence,
  });
  const pending = events.subscribe(service, {
    name: "consumer",
    filter: {},
    limit: 2,
  });
  assert.deepEqual(
    events.ack(service, {
      name: "consumer",
      throughSequence: first.throughSequence,
    }),
    { acknowledgedSequence: first.throughSequence },
  );
  const current = { ...service, generation: 2 };
  assert.deepEqual(
    events.subscribe(current, { name: "consumer", filter: {}, limit: 2 }),
    pending,
  );
  const unmatched = events.subscribe(current, {
    name: "empty-filter",
    filter: { topics: ["never"] },
  });
  assert.equal(unmatched.items.length, 0);
  assert.equal(unmatched.throughSequence, 5);
  events.ack(current, { name: "empty-filter", throughSequence: 5 });
  const idle = events.subscribe(current, {
    name: "idle",
    filter: {},
    initialSequence: 5,
  });
  assert.equal(idle.items.length, 0);
  events.ack(current, { name: "idle", throughSequence: idle.throughSequence });
  writeText("after-idle");
  assert.equal(
    events.subscribe(current, { name: "idle", filter: {} }).items.length,
    1,
  );
});

test("schema admission rejects unsafe graphs and handles bounded structural recursion and linear regex", () => {
  const validators = new SchemaValidators();
  for (const schema of [
    { $ref: "https://example.invalid/schema" },
    { $ref: "#/$defs/x", $defs: { x: { $ref: "#/$defs/x" } } },
    { $dynamicRef: "#" },
    { customCode: "danger" },
    { pattern: "(a)\\1" },
  ])
    assert.throws(() => validators.compile(schema));
  const recursive = {
    type: "object",
    properties: { child: { $ref: "#/$defs/node" } },
    $defs: {
      node: {
        type: "object",
        properties: { child: { $ref: "#/$defs/node" } },
        additionalProperties: false,
      },
    },
    additionalProperties: false,
  };
  validators.validate(recursive, { child: { child: {} } });
  assert.throws(() => validators.validate(recursive, { child: { extra: 1 } }));
  validators.validate({ type: "string", pattern: "^(a+)+$" }, "a".repeat(1000));
  assert.throws(() =>
    validators.validate(
      { type: "string", pattern: "^(a+)+$" },
      "a".repeat(1000) + "!",
    ),
  );
});

test("SQLite-safe backup retains immutable history, ledger and events; restored nodes start offline", async (t) => {
  const { store, objects, directory, writeText } = fixture(t);
  const first = writeText("backup");
  store.run(
    "INSERT INTO service_nodes(id,principal_id,host_id,service_name,generation,status_json) VALUES (?,?,?,?,1,?)",
    "backup-node",
    credential.principalId,
    "host",
    "service",
    canonical({ connected: true, ready: true, synced: true }),
  );
  const path = join(directory, "restored.sqlite");
  const result = await store.backupTo(path);
  assert.ok(result.pages > 0);
  const restored = new HiveStore(path);
  try {
    assert.deepEqual(
      new Objects(restored).read({ objectId: first.object.id }),
      objects.read({ objectId: first.object.id }),
    );
    assert.equal(restored.get("SELECT count(*) AS n FROM events")!["n"], 1);
    assert.equal(restored.get("SELECT count(*) AS n FROM mutations")!["n"], 2);
    assert.equal(
      JSON.parse(
        String(
          restored.get("SELECT status_json FROM service_nodes")!["status_json"],
        ),
      ).connected,
      false,
    );
  } finally {
    restored.close();
  }
});
