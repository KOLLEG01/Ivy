import test from "node:test";
import assert from "node:assert/strict";
import { HiveStore } from "../services/hive/src/store.js";
import { Objects } from "../services/hive/src/objects.js";
import { pruneObjectRevisions } from "../services/hive/src/object-retention.js";
import { operationId } from "../packages/contracts/src/operation-id.js";
import { digest } from "../packages/contracts/src/canonical.js";

function fixture() {
  const store = new HiveStore(":memory:"),
    objects = new Objects(store);
  const context = {
    principalId: "collector-test",
    credentialDigest: digest("collector-test"),
    serviceNodeId: "test.collector",
    generation: 7,
  };
  store.configureCredentials([
    { principalId: context.principalId, digest: context.credentialDigest },
  ]);
  store.run(
    "INSERT INTO service_nodes VALUES (?,?,?,?,?,?)",
    context.serviceNodeId,
    context.principalId,
    "test",
    "data-collector",
    1,
    "{}",
  );
  objects.register(
    {
      key: "data-collector/test-result",
      version: "1.0.0",
      owner: { kind: "service", serviceName: "data-collector" },
      mediaType: "text/plain",
      retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
      specMarkdown: "Test result history.",
    },
    { kind: "service", serviceName: "data-collector" },
  );
  let nonce = 0;
  const mutationId = () =>
    operationId(store.runtimeEpoch, Date.now(), "test-" + ++nonce);
  let saved = objects.write(context, {
    mutationId: mutationId(),
    contractVersion: "1.0.0",
    create: {
      parentId: null,
      ownerObjectId: null,
      name: "result",
      contractKey: "data-collector/test-result",
    },
    references: {},
    content: { encoding: "text", value: "1234567890" },
  });
  const write = () => {
    saved = objects.write(context, {
      objectId: saved.object.id,
      expectedRevision: saved.revision.revision,
      mutationId: mutationId(),
      contractVersion: "1.0.0",
      references: {},
      content: { encoding: "text", value: "1234567890" },
    });
  };
  const prune = (extra = {}) =>
    pruneObjectRevisions(store, context, {
      objectId: saved.object.id,
      expectedRevision: saved.revision.revision,
      mutationId: mutationId(),
      maximumCount: 1000,
      maximumAgeDays: 7,
      maximumBytes: 10000,
      ...extra,
    });
  return {
    store,
    objects,
    context,
    mutationId,
    write,
    prune,
    get objectId() {
      return saved.object.id;
    },
  };
}
test("collector retention applies count and byte budgets while preserving latest revision", () => {
  const f = fixture();
  try {
    for (let i = 0; i < 4; i++) f.write();
    f.store.changes.clear();
    assert.deepEqual(f.prune({ maximumCount: 3, maximumBytes: 25 }), {
      deleted: 3,
      protected: 0,
      remainingBytes: 20,
    });
    assert.deepEqual([...f.store.changes], ["objects/data-collector/test-result", "system"]);
    assert.equal(f.objects.read({ objectId: f.objectId }).revision.revision, 5);
    assert.equal(f.objects.history({ objectId: f.objectId }).items.length, 2);
    assert.equal(f.prune({ maximumBytes: 1 }).remainingBytes, 10);
  } finally {
    f.store.close();
  }
});

test("collector retention deletes bounded batches and preserves replay without repeated invalidation", (t) => {
  const f = fixture();
  t.after(() => f.store.close());
  for (let i = 1; i < 1003; i++) f.write();
  const original = f.store.all.bind(f.store);
  t.mock.method(f.store, "all", (...args: Parameters<typeof f.store.all>) => {
    const rows = original(...args);
    assert.ok(rows.length <= 501, "pruning must not materialize the entire revision history");
    return rows;
  });
  const request = { maximumCount: 1, mutationId: f.mutationId() };
  assert.deepEqual(f.prune(request), { deleted: 500, protected: 0, remainingBytes: 5030 });
  f.store.changes.clear();
  assert.deepEqual(f.prune(request), { deleted: 500, protected: 0, remainingBytes: 5030 });
  assert.equal(f.store.changes.size, 0);
  assert.equal(f.prune({ maximumCount: 1 }).deleted, 500);
  assert.deepEqual(f.prune({ maximumCount: 1 }), { deleted: 2, protected: 0, remainingBytes: 10 });
});
test("collector retention makes progress under a bounded selection budget for a large backlog", (t) => {
  const f = fixture(), count = 10003;
  t.after(() => f.store.close());
  f.store.transaction(() => {
    f.store.run(
      `WITH RECURSIVE seq(n) AS (SELECT 2 UNION ALL SELECT n+1 FROM seq WHERE n<?)
       INSERT INTO revisions SELECT r.object_id,n,r.contract_key,r.contract_version,
         r.encoding,r.content,r.content_hash,r.byte_length,r.created_at,r.references_json
       FROM seq JOIN revisions r ON r.object_id=? AND r.revision=1`,
      count, f.objectId,
    );
    f.store.run("UPDATE objects SET current_revision=? WHERE id=?", count, f.objectId);
  });
  const check = f.store.checkBudget.bind(f.store);
  let checks = 0;
  t.mock.method(f.store, "checkBudget", () => {
    assert.ok(++checks <= 1500, "candidate selection must not visit every historical revision");
    check();
  });
  let remainingBytes = count * 10;
  for (const limits of [
    { maximumCount: 1, maximumBytes: 1_000_000 },
    { maximumCount: 100000, maximumBytes: 20 },
  ]) {
    checks = 0;
    remainingBytes -= 5000;
    assert.deepEqual(f.store.withBudget(() => f.prune({ expectedRevision: count, ...limits })), {
      deleted: 500, protected: 0, remainingBytes,
    });
  }
  assert.equal(f.objects.read({ objectId: f.objectId }).revision.revision, count);
});

test("collector retention expires old revisions and preserves reference targets", (t) => {
  const now = Date.now();
  t.mock.timers.enable({ apis: ["Date"], now: now - 14 * 86400000 });
  const f = fixture();
  try {
    t.mock.timers.setTime(now);
    f.write();
    f.write();
    f.objects.write(f.context, {
      mutationId: f.mutationId(),
      contractVersion: "1.0.0",
      references: { snapshot: { objectId: f.objectId, revision: 2 } },
      create: {
        parentId: null,
        ownerObjectId: null,
        name: "pinned",
        contractKey: "data-collector/test-result",
      },
      content: { encoding: "text", value: "pin" },
    });
    assert.deepEqual(f.prune(), {
      deleted: 1,
      protected: 0,
      remainingBytes: 20,
    });
    assert.deepEqual(f.prune({ maximumCount: 1 }), {
      deleted: 0,
      protected: 1,
      remainingBytes: 20,
    });
    assert.equal(
      f.objects.read({ objectId: f.objectId, revision: 2 }).revision.revision,
      2,
    );
  } finally {
    f.store.close();
  }
});
test("collector retention rejects stale pins and foreign services without deleting history", () => {
  const f = fixture();
  try {
    f.write();
    assert.throws(() => f.prune({ expectedRevision: 1 }), /changed/);
    f.store.run(
      "UPDATE service_nodes SET service_name='foreign' WHERE id=?",
      f.context.serviceNodeId,
    );
    assert.throws(() => f.prune({ maximumCount: 1 }), /owning service/);
    assert.equal(f.objects.history({ objectId: f.objectId }).items.length, 2);
  } finally {
    f.store.close();
  }
});
