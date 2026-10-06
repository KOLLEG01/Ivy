import test from "node:test";
import assert from "node:assert/strict";
import { operationId } from "../packages/contracts/src/operation-id.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import type {
  DataContract,
  RevisionReferences,
} from "../packages/contracts/src/types.js";
import { digest } from "../packages/contracts/src/canonical.js";
import {
  Events,
  maximumSubscriptionsPerNode,
} from "../services/hive/src/events.js";
import { Objects } from "../services/hive/src/objects.js";
import { Retention } from "../services/hive/src/retention.js";
import { EVENT_MAX_COUNT, HiveStore } from "../services/hive/src/store.js";

const context = {
  principalId: "retention-test",
  credentialDigest: digest("retention-test"),
};
const policy = (
  objects: "retain" | "owned" | DataContract["retention"]["objects"],
  revisions: DataContract["retention"]["revisions"],
): DataContract["retention"] => ({
  objects: typeof objects === "string" ? { mode: objects } : objects,
  revisions,
});
const contract = (
  key: string,
  retention: DataContract["retention"],
): DataContract => ({
  key,
  version: "1.0.0",
  owner: { kind: "agent" },
  mediaType: "text/plain",
  retention,
  specMarkdown: "Retention test domain content.",
});
const code = (value: string) => (error: unknown) =>
  error instanceof IvyError && error.code === value;

function fixture() {
  const store = new HiveStore(":memory:");
  store.configureCredentials([
    { principalId: context.principalId, digest: context.credentialDigest },
  ]);
  const objects = new Objects(store),
    retention = new Retention(store),
    events = new Events(store);
  let nonce = 0;
  const id = (at = Date.now()) =>
    operationId(store.runtimeEpoch, at, "test-" + ++nonce);
  const register = (definition: DataContract) =>
    objects.registerAgent(context, { definition, mutationId: id() });
  const write = (
    key: string,
    name: string,
    ownerObjectId: string | null,
    references: RevisionReferences = {},
  ) =>
    objects.write(context, {
      mutationId: id(),
      contractVersion: "1.0.0",
      references,
      create: { contractKey: key, parentId: null, ownerObjectId, name },
      content: { encoding: "text", value: name },
    });
  return { store, objects, retention, events, id, register, write };
}

test("family policies, immutable owners and exact reference targets are enforced", () => {
  const f = fixture();
  try {
    f.register(
      contract(
        "test/retained",
        policy("retain", { mode: "bounded", maximumCount: 2 }),
      ),
    );
    f.register(contract("test/artifact", policy("owned", { mode: "current" })));
    const owner = f.write("test/retained", "owner", null);
    assert.throws(
      () => f.write("test/retained", "bad-retained", owner.object.id),
      code("invalid_arguments"),
    );
    assert.throws(
      () => f.write("test/artifact", "missing-owner", null),
      code("invalid_arguments"),
    );
    const artifact = f.write("test/artifact", "artifact", owner.object.id);
    assert.throws(
      () => f.write("test/artifact", "owned-owner", artifact.object.id),
      code("invalid_arguments"),
    );
    assert.throws(
      () =>
        f.objects.write(context, {
          mutationId: f.id(),
          contractVersion: "1.0.0",
          references: { missing: { objectId: "missing", revision: 1 } },
          create: {
            contractKey: "test/retained",
            parentId: null,
            ownerObjectId: null,
            name: "missing-pin",
          },
          content: { encoding: "text", value: "x" },
        }),
      code("not_found"),
    );
    assert.throws(
      () =>
        f.objects.write(context, {
          mutationId: f.id(),
          contractVersion: "1.0.0",
          references: { self: { objectId: owner.object.id, revision: 1 } },
          objectId: owner.object.id,
          expectedRevision: 1,
          content: { encoding: "text", value: "x" },
        }),
      code("invalid_arguments"),
    );
    assert.equal(artifact.object.ownerObjectId, owner.object.id);
  } finally {
    f.store.close();
  }
});

test("collector previews new policy, keeps pins, then prunes history and abandoned owned artifacts", () => {
  const f = fixture();
  try {
    f.register(
      contract(
        "test/page",
        policy("retain", { mode: "bounded", maximumCount: 2 }),
      ),
    );
    f.register(contract("test/artifact", policy("owned", { mode: "current" })));
    let page = f.write("test/page", "page", null);
    const artifact = f.write("test/artifact", "artifact", page.object.id);
    f.store.run(
      "UPDATE objects SET created_at=? WHERE id=?",
      new Date(Date.now() - 25 * 60 * 60 * 1000).toISOString(),
      artifact.object.id,
    );
    for (let revision = 2; revision <= 4; revision++)
      page = f.objects.write(context, {
        mutationId: f.id(),
        contractVersion: "1.0.0",
        objectId: page.object.id,
        expectedRevision: revision - 1,
        references:
          revision === 4
            ? { artifact: { objectId: artifact.object.id, revision: 1 } }
            : {},
        content: { encoding: "text", value: "revision-" + revision },
      });
    const preview = f.retention.collect();
    assert.equal(
      preview.families.find((value) => value.contractKey === "test/page")!
        .previewRequired,
      false,
    );
    assert.equal(
      f.store.get(
        "SELECT COUNT(*) AS count FROM revisions WHERE object_id=?",
        page.object.id,
      )!["count"],
      4,
    );
    f.retention.collect();
    assert.deepEqual(
      f.objects
        .history({ objectId: page.object.id })
        .items.map((value) => value.revision),
      [4, 3],
    );
    assert.equal(
      f.objects.history({ objectId: page.object.id }).historyComplete,
      false,
    );
    assert.throws(
      () => f.objects.read({ objectId: page.object.id, revision: 2 }),
      code("revision_pruned"),
    );
    assert.equal(
      f.objects.read({ objectId: artifact.object.id }).content.encoding,
      "text",
    );
    page = f.objects.write(context, {
      mutationId: f.id(),
      contractVersion: "1.0.0",
      objectId: page.object.id,
      expectedRevision: 4,
      references: {},
      content: { encoding: "text", value: "revision-5" },
    });
    f.retention.collect();
    assert.equal(
      f.objects.stat({ objectId: artifact.object.id }).id,
      artifact.object.id,
      "the still-retained referring revision protects the artifact",
    );
    page = f.objects.write(context, {
      mutationId: f.id(),
      contractVersion: "1.0.0",
      objectId: page.object.id,
      expectedRevision: 5,
      references: {},
      content: { encoding: "text", value: "revision-6" },
    });
    f.retention.collect();
    f.retention.collect();
    assert.throws(
      () => f.objects.stat({ objectId: artifact.object.id }),
      code("not_found"),
    );
    assert.deepEqual(
      f.objects
        .history({ objectId: page.object.id })
        .items.map((value) => value.revision),
      [6, 5],
    );
  } finally {
    f.store.close();
  }
});

test("expired domain objects and their owned artifacts are deleted as one referenced set", () => {
  const f = fixture(),
    old = new Date(Date.now() - 91 * 24 * 60 * 60 * 1000).toISOString();
  try {
    f.register(contract("test/root", policy("retain", { mode: "current" })));
    for (const key of ["test/input", "test/result", "test/reply"])
      f.register(
        contract(
          key,
          policy({ mode: "expire", maximumAgeDays: 90 }, { mode: "current" }),
        ),
      );
    f.register(contract("test/artifact", policy("owned", { mode: "current" })));
    const root = f.write("test/root", "root", null);
    let input = f.write("test/input", "input", null);
    const artifact = f.objects.write(context, {
      mutationId: f.id(),
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: "test/artifact",
        parentId: input.object.id,
        ownerObjectId: input.object.id,
        name: "artifact",
      },
      content: { encoding: "text", value: "artifact" },
    });
    const result = f.write("test/result", "result", null, {
      artifact: { objectId: artifact.object.id, revision: 1 },
    });
    input = f.objects.write(context, {
      mutationId: f.id(),
      contractVersion: "1.0.0",
      objectId: input.object.id,
      expectedRevision: 1,
      references: { result: { objectId: result.object.id, revision: 1 } },
      content: { encoding: "text", value: "completed" },
    });
    const reply = f.write("test/reply", "reply", null, {
      input: { objectId: input.object.id, revision: 2 },
      result: { objectId: result.object.id, revision: 1 },
    });
    f.store.run(
      "UPDATE objects SET created_at=?,updated_at=? WHERE id IN (?,?,?,?)",
      old,
      old,
      input.object.id,
      result.object.id,
      reply.object.id,
      artifact.object.id,
    );
    f.retention.collect();
    assert.equal(
      f.retention
        .status()
        .families.find((value) => value.contractKey === "test/input")!
        .eligibleObjectCount,
      1,
    );
    f.retention.collect();
    assert.equal(
      f.objects.stat({ objectId: root.object.id }).id,
      root.object.id,
    );
    for (const objectId of [
      input.object.id,
      result.object.id,
      reply.object.id,
      artifact.object.id,
    ])
      assert.throws(() => f.objects.stat({ objectId }), code("not_found"));
  } finally {
    f.store.close();
  }
});

test("object expiry is based on the latest update and keeps externally referenced records", () => {
  const f = fixture(),
    old = new Date(Date.now() - 91 * 24 * 60 * 60 * 1000).toISOString();
  try {
    f.register(
      contract(
        "test/expiring",
        policy({ mode: "expire", maximumAgeDays: 90 }, { mode: "current" }),
      ),
    );
    f.register(
      contract("test/retained", policy("retain", { mode: "current" })),
    );
    const fresh = f.write("test/expiring", "fresh", null),
      pinned = f.write("test/expiring", "pinned", null);
    f.store.run(
      "UPDATE objects SET created_at=? WHERE id=?",
      old,
      fresh.object.id,
    );
    f.store.run(
      "UPDATE objects SET created_at=?,updated_at=? WHERE id=?",
      old,
      old,
      pinned.object.id,
    );
    f.write("test/retained", "source", null, {
      pinned: { objectId: pinned.object.id, revision: 1 },
    });
    f.retention.collect();
    f.retention.collect();
    assert.equal(
      f.objects.stat({ objectId: fresh.object.id }).id,
      fresh.object.id,
    );
    assert.equal(
      f.objects.stat({ objectId: pinned.object.id }).id,
      pinned.object.id,
    );
  } finally {
    f.store.close();
  }
});

test("object collection reapplies dependency protection after policy previews are filtered", () => {
  const f = fixture(),
    old = new Date(Date.now() - 2 * 24 * 60 * 60 * 1000).toISOString(),
    expiring = policy(
      { mode: "expire", maximumAgeDays: 1 },
      { mode: "current" },
    );
  try {
    f.register(contract("test/target", expiring));
    f.register(contract("test/source", policy("retain", { mode: "current" })));
    const target = f.write("test/target", "target", null),
      source = f.write("test/source", "source", null, {
        target: { objectId: target.object.id, revision: 1 },
      });
    f.store.run(
      "UPDATE objects SET created_at=?,updated_at=? WHERE id IN (?,?)",
      old,
      old,
      target.object.id,
      source.object.id,
    );
    f.retention.collect();
    f.register({
      ...contract("test/source", expiring),
      version: "2.0.0",
    });
    f.retention.collect();
    assert.equal(
      f.objects.stat({ objectId: target.object.id }).id,
      target.object.id,
    );
    assert.equal(
      f.objects.stat({ objectId: source.object.id }).id,
      source.object.id,
    );
    f.retention.collect();
    assert.throws(
      () => f.objects.stat({ objectId: target.object.id }),
      code("not_found"),
    );
    assert.throws(
      () => f.objects.stat({ objectId: source.object.id }),
      code("not_found"),
    );
  } finally {
    f.store.close();
  }
});

test("legacy retention usage is fully initialized before new writes adjust it", () => {
  const f = fixture(),
    key = "test/usage";
  try {
    f.register(contract(key, policy("retain", { mode: "all" })));
    for (let index = 0; index < 11; index++)
      f.write(key, "existing-" + index, null);
    const initial = f.retention
      .collect()
      .families.find((value) => value.contractKey === key)!;
    f.store.run(
      "UPDATE retention_families SET summary_json=? WHERE contract_key=?",
      JSON.stringify({
        checkedAt: new Date().toISOString(),
        eligibleObjects: 0,
        eligibleRevisions: 0,
        protectedRevisions: 0,
      }),
      key,
    );
    f.write(key, "new-after-upgrade", null);
    const initialized = f.retention
      .collect()
      .families.find((value) => value.contractKey === key)!;
    assert.equal(initial.previewRequired, false);
    assert.equal(initialized.objectCount, 12);
    assert.equal(initialized.revisionCount, 12);
    f.write(key, "new-after-initialization", null);
    const adjusted = f.retention
      .status()
      .families.find((value) => value.contractKey === key)!;
    assert.equal(adjusted.objectCount, 13);
    assert.equal(adjusted.revisionCount, 13);
  } finally {
    f.store.close();
  }
});

test("epoch replay window and global event prefix gap remain explicit after collection", () => {
  const f = fixture();
  try {
    f.register(contract("test/page", policy("retain", { mode: "current" })));
    const page = f.write("test/page", "page", null);
    assert.throws(
      () =>
        f.objects.archive(context, {
          objectId: page.object.id,
          archived: true,
          mutationId: operationId("other-epoch", Date.now(), "wrong"),
        }),
      code("mutation_expired"),
    );
    assert.throws(
      () =>
        f.objects.archive(context, {
          objectId: page.object.id,
          archived: true,
          mutationId: f.id(Date.now() - 24 * 60 * 60 * 1000),
        }),
      code("mutation_expired"),
    );
    f.store.run(
      "INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES (?,?,?,?,?,?)",
      "hive.object.changed",
      "1.0.0",
      "test",
      new Date(Date.now() - 8 * 24 * 60 * 60 * 1000).toISOString(),
      f.id(),
      "{}",
    );
    const sequence = Number(
      f.store.get("SELECT MAX(sequence) AS value FROM events")!["value"],
    );
    f.retention.collect();
    const batch = f.events.read({ afterSequence: 0, filter: {} });
    assert.deepEqual(batch.gap, {
      prunedThroughSequence: sequence,
      resumeAfterSequence: sequence,
    });
    assert.equal(batch.throughSequence, sequence);
  } finally {
    f.store.close();
  }
});

test("durable Secretary cursors retain old events and reserve room to acknowledge a full journal", () => {
  const f = fixture();
  try {
    const nodeId = "retention-secretary";
    f.store.run("INSERT INTO service_nodes VALUES (?,?,?,?,?,?)", nodeId, context.principalId, "host", "secretary", 1,
      JSON.stringify({ serviceNodeId: nodeId, principalId: context.principalId }));
    const secretary = { ...context, serviceNodeId: nodeId, generation: 1 };
    const idle = f.events.subscribe(secretary, { name: "secretary-events", filter: { topics: ["hive.object.changed"] }, durable: true });
    f.events.ack(secretary, { name: "secretary-events", throughSequence: idle.throughSequence });
    f.store.run(`WITH RECURSIVE sequence(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM sequence WHERE value<?)
      INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload)
      SELECT 'hive.object.changed','1.0.0','test',?, 'old-'||value,'{}' FROM sequence`,
      EVENT_MAX_COUNT + 1, new Date(Date.now() - 8 * 24 * 60 * 60 * 1000).toISOString());
    f.retention.collect();
    assert.equal(f.store.get("SELECT COUNT(*) AS count FROM events")!["count"], EVENT_MAX_COUNT + 1);
    assert.throws(() => f.store.admitEvent(), code("capacity_exceeded"));
    assert.doesNotThrow(() => f.store.admitEvent(true), "Secretary needs headroom to save executions before ack");
    const first = f.events.subscribe(secretary, { name: "secretary-events", filter: { topics: ["hive.object.changed"] }, durable: true });
    assert.equal(first.items.length, 100);
    assert.equal(first.gap, null);
    f.events.ack(secretary, { name: "secretary-events", throughSequence: first.throughSequence });
    assert.equal(f.store.get("SELECT COUNT(*) AS count FROM events")!["count"], EVENT_MAX_COUNT - 99);
    assert.doesNotThrow(() => f.store.admitEvent());
    const second = f.events.subscribe(secretary, { name: "secretary-events", filter: { topics: ["hive.object.changed"] }, durable: true });
    f.events.ack(secretary, { name: "secretary-events", throughSequence: second.throughSequence });
    assert.doesNotThrow(() => f.store.admitEvent());
    assert.equal(second.items[0]?.sequence, 101);
  } finally {
    f.store.close();
  }
});

test("collected mutation cutoff remains exclusive after a clock rollback", () => {
  const f = fixture(),
    originalNow = Date.now,
    issuedAt = 1_800_000_000_000;
  try {
    Date.now = () => issuedAt;
    f.register(contract("test/page", policy("retain", { mode: "current" })));
    const page = f.write("test/page", "page", null);
    const mutationId = f.id(issuedAt);
    f.objects.archive(context, {
      objectId: page.object.id,
      archived: true,
      mutationId,
    });
    f.store.cleanupMutationReceipts(issuedAt + 24 * 60 * 60 * 1000);
    assert.equal(
      f.store.get("SELECT 1 FROM mutations WHERE mutation_id=?", mutationId),
      undefined,
    );
    Date.now = () => issuedAt + 24 * 60 * 60 * 1000 - 1;
    assert.throws(
      () =>
        f.objects.archive(context, {
          objectId: page.object.id,
          archived: true,
          mutationId,
        }),
      code("mutation_expired"),
    );
  } finally {
    Date.now = originalNow;
    f.store.close();
  }
});

test("bounded history uses revision numbers after an earlier collection leaves a hole", () => {
  const f = fixture();
  try {
    f.register(contract("test/page", policy("retain", { mode: "current" })));
    let page = f.write("test/page", "page", null);
    for (let revision = 2; revision <= 4; revision++)
      page = f.objects.write(context, {
        mutationId: f.id(),
        contractVersion: "1.0.0",
        objectId: page.object.id,
        expectedRevision: revision - 1,
        references: {},
        content: { encoding: "text", value: "revision-" + revision },
      });
    f.retention.collect();
    f.retention.collect();
    assert.deepEqual(
      f.objects
        .history({ objectId: page.object.id })
        .items.map((value) => value.revision),
      [4],
    );
    f.register({
      ...contract(
        "test/page",
        policy("retain", { mode: "bounded", maximumCount: 2 }),
      ),
      version: "2.0.0",
    });
    f.store.run(
      "INSERT INTO revisions VALUES(?,?,?,?,?,?,?,?,?,?)",
      page.object.id,
      1,
      "test/page",
      "1.0.0",
      "text",
      "page",
      digest("page"),
      4,
      new Date().toISOString(),
      "{}",
    );
    f.retention.collect();
    f.retention.collect();
    assert.deepEqual(
      f.objects
        .history({ objectId: page.object.id })
        .items.map((value) => value.revision),
      [4],
    );
  } finally {
    f.store.close();
  }
});

test("large revision histories make bounded progress across passes and keep protected revisions", (t) => {
  const f = fixture();
  try {
    f.register(contract("test/large", policy("retain", { mode: "current" })));
    f.register(contract("test/source", policy("retain", { mode: "current" })));
    const page = f.write("test/large", "large", null),
      createdAt = new Date().toISOString();
    f.store.transaction(() => {
      for (let revision = 2; revision <= 1202; revision++)
        f.store.run(
          "INSERT INTO revisions VALUES(?,?,?,?,?,?,?,?,?,?)",
          page.object.id,
          revision,
          "test/large",
          "1.0.0",
          "text",
          "revision-" + revision,
          digest("revision-" + revision),
          String("revision-" + revision).length,
          createdAt,
          "{}",
        );
      f.store.run(
        "UPDATE objects SET current_revision=?,updated_at=? WHERE id=?",
        1202,
        createdAt,
        page.object.id,
      );
    });
    f.write("test/source", "source", null, {
      protected: { objectId: page.object.id, revision: 100 },
    });
    f.retention.collect();
    assert.equal(
      Number(
        f.store.get(
          "SELECT COUNT(*) AS count FROM revisions WHERE object_id=?",
          page.object.id,
        )!["count"],
      ),
      1202,
      "the first policy pass is preview-only",
    );
    const originalAll = f.store.all.bind(f.store);
    const originalGet = f.store.get.bind(f.store);
    let materializedRevisionRows = 0,
      fullHistoryAggregates = 0;
    t.mock.method(f.store, "all", (...args: Parameters<typeof f.store.all>) => {
      const [sql] = args,
        rows = originalAll(...args);
      if (/FROM revisions r/.test(sql)) materializedRevisionRows += rows.length;
      return rows;
    });
    t.mock.method(f.store, "get", (...args: Parameters<typeof f.store.get>) => {
      if (/COUNT\(\*\).*FROM revisions WHERE contract_key/.test(args[0]))
        fullHistoryAggregates++;
      return originalGet(...args);
    });
    f.retention.collect();
    assert.ok(
      materializedRevisionRows <= 501,
      `one active retention pass materialized ${materializedRevisionRows} revision candidates`,
    );
    assert.equal(
      fullHistoryAggregates,
      0,
      "active collection and its returned status do not aggregate the full history",
    );
    assert.equal(
      Number(
        f.store.get(
          "SELECT COUNT(*) AS count FROM revisions WHERE object_id=?",
          page.object.id,
        )!["count"],
      ),
      702,
    );
    f.retention.collect();
    assert.equal(
      Number(
        f.store.get(
          "SELECT COUNT(*) AS count FROM revisions WHERE object_id=?",
          page.object.id,
        )!["count"],
      ),
      202,
    );
    f.retention.collect();
    assert.deepEqual(
      f.objects
        .history({ objectId: page.object.id, limit: 10 })
        .items.map((value) => value.revision),
      [1202, 100],
    );
    f.retention.collect();
    assert.deepEqual(
      f.objects
        .history({ objectId: page.object.id, limit: 10 })
        .items.map((value) => value.revision),
      [1202, 100],
    );
  } finally {
    f.store.close();
  }
});

test("Secretary checkpoints do not replenish the journal they drain", () => {
  const f = fixture();
  try {
    const count = () => f.store.get("SELECT COUNT(*) AS count FROM events")!["count"];
    const before = count();
    for (const key of ["secretary/execution", "secretary/schedule-progress", "secretary/event-progress"])
      f.store.appendCoreEvent(context, "internal", 1, "delete", "internal-" + key, new Date().toISOString(), key);
    assert.equal(count(), before);
    f.store.appendCoreEvent(context, "assignment", 1, "delete", "assignment-change", new Date().toISOString(), "secretary/assignment");
    assert.equal(Number(count()), Number(before) + 1);
  } finally { f.store.close(); }
});

test("subscriptions are bounded per node and retired with their configured principal", () => {
  const f = fixture();
  try {
    f.store.run(
      "INSERT INTO service_nodes VALUES (?,?,?,?,?,?)",
      "consumer-node",
      context.principalId,
      "host",
      "consumer",
      1,
      JSON.stringify({
        serviceNodeId: "consumer-node",
        principalId: context.principalId,
      }),
    );
    const consumer = {
      ...context,
      serviceNodeId: "consumer-node",
      generation: 1,
    };
    for (let index = 0; index < maximumSubscriptionsPerNode; index++)
      f.events.subscribe(consumer, {
        name: "subscription-" + index,
        filter: {},
      });
    assert.throws(
      () => f.events.subscribe(consumer, { name: "overflow", filter: {} }),
      code("capacity_exceeded"),
    );
    f.store.configureCredentials([
      { principalId: "replacement", digest: digest("replacement") },
    ]);
    assert.equal(
      f.store.get(
        "SELECT COUNT(*) AS count FROM subscriptions WHERE node_id=?",
        "consumer-node",
      )!["count"],
      0,
    );
  } finally {
    f.store.close();
  }
});

test("retention usage stays integral and revision batches roll back with their summary", (t) => {
  const f = fixture();
  t.after(() => f.store.close());
  f.register(contract("test/atomic", policy("retain", { mode: "current" })));
  const page = f.write("test/atomic", "atomic", null);
  f.retention.collect();
  for (let revision = 1; revision < 3; revision++) f.objects.write(context, {
    objectId: page.object.id, expectedRevision: revision, mutationId: f.id(),
    contractVersion: "1.0.0", references: {}, content: { encoding: "text", value: "next" },
  });
  const summary = () => f.store.get("SELECT summary_json,json_type(summary_json,'$.revisionCount') AS kind FROM retention_families WHERE contract_key='test/atomic'")!;
  assert.equal(summary()["kind"], "integer");
  assert.equal(JSON.parse(String(summary()["summary_json"])).revisionCount, 3);
  const run = f.store.run.bind(f.store);
  const failure = t.mock.method(f.store, "run", (...args: Parameters<typeof f.store.run>) => {
    if (args[0].startsWith("INSERT INTO retention_families")) throw new Error("summary unavailable");
    return run(...args);
  });
  f.store.changes.clear();
  assert.throws(() => f.retention.collect(), /summary unavailable/);
  assert.equal(f.objects.history({ objectId: page.object.id }).items.length, 3);
  assert.equal(f.store.changes.size, 0);
  failure.mock.restore();
  f.retention.collect();
  assert.equal(f.objects.history({ objectId: page.object.id }).items.length, 1);
  assert.equal(JSON.parse(String(summary()["summary_json"])).revisionCount, 1);
  assert.ok(f.store.changes.has("objects/test/atomic"));
  assert.ok(f.store.changes.has("system"));
});

test("expired object collection invalidates UI reads without adding durable events", (t) => {
  const f = fixture();
  t.after(() => f.store.close());
  f.register(contract("test/expiring", policy({ mode: "expire", maximumAgeDays: 1 }, { mode: "all" })));
  const page = f.write("test/expiring", "expired", null);
  const eventCount = f.store.get("SELECT COUNT(*) AS count FROM events")!["count"];
  const future = Date.now() + 2 * 86400000;
  f.retention.collect(future);
  f.store.changes.clear();
  f.retention.collect(future);
  assert.equal(f.store.get("SELECT 1 FROM objects WHERE id=?", page.object.id), undefined);
  assert.ok(f.store.changes.has("objects"));
  assert.ok(f.store.changes.has("system"));
  assert.equal(f.store.get("SELECT COUNT(*) AS count FROM events")!["count"], eventCount);
});
