import test from "node:test";
import assert from "node:assert/strict";
import { AssignmentScheduler } from "../dist/services/secretary/src/assignment-scheduler.js";
import {
  objectProjection,
  observationReady,
  pointer,
} from "../dist/services/secretary/src/assignment-object.js";
import { validateAssignment } from "../dist/services/secretary/src/assignment-schema.js";
import { assignmentFor } from "./secretary-fixture.mjs";
import { SecretaryStore } from "../dist/services/secretary/src/store.js";
import { canonical, hashJson } from "../dist/packages/sdk/src/node.js";
import { resolve } from "node:path";

const at = "2026-10-07T08:00:00.000Z";
const trigger = {
  kind: "object-change",
  objectIds: ["result"],
  paths: ["/data/activity"],
  delaySeconds: 300,
  observation: {
    completePath: "/data/observation/complete",
    observedAtPath: "/data/observation/observedAt",
    maximumAgeSeconds: 180,
  },
};
const selected = {
  pin: { objectId: "assignment", revision: 1 },
  value: assignmentFor(at, { enabled: true, trigger }),
};
const content = (ids, observedAt = at, complete = true) => ({
  data: { activity: ids, observation: { complete, observedAt } },
});

test("object executions retain and validate both input revisions across serialized storage", (t) => {
  const store = new SecretaryStore({}, "root");
  t.after(() => store.close());
  const value = {
    schemaVersion: 1,
    executionId: hashJson("object-test"),
    assignment: selected.pin,
    assignmentSnapshot: selected.value,
    trigger: {
      kind: "event",
      key: "object-batch:4:result",
      occurredAt: at,
      payload: {
        kind: "secretary.object.batch",
        objectId: "result",
        beforeRevision: 2,
        revision: 4,
        before: { "/data/activity": [] },
        after: { "/data/activity": ["one"] },
      },
    },
    effectiveRules: {
      main: { enabled: false, minimumUrgency: "normal", window: null },
      whatsapp: {
        enabled: false,
        minimumUrgency: "normal",
        questionsOnly: false,
        window: null,
      },
      voice: {
        enabled: false,
        minimumUrgency: "critical",
        immediateOnly: true,
        window: null,
      },
      researchMaxMinutes: 0,
    },
    executionTarget: {
      serviceNodeId: "agent",
      threadCwd: resolve("secretary-test-project"),
      model: null,
      effort: "medium",
      permissions: ":read-only",
    },
    phase: "queued",
    serviceNodeId: "agent",
    nativeTarget: null,
    threadId: null,
    turnId: null,
    nativeOperations: {
      threadStart: null,
      turnStart: null,
      archive: null,
      delete: null,
    },
    preflightOutput: null,
    result: null,
    errorCode: null,
    createdAt: at,
    startedAt: null,
    completedAt: null,
    archivedAt: null,
    deleteAfter: null,
    deletedAt: null,
    updatedAt: at,
  };
  const write = store.objectWriteRequest(
    "secretary/execution",
    value,
    { createName: "execution" },
    "00000000-0000-4000-8000-000000000001:" + Date.parse(at) + ":pin-write",
  );
  assert.deepEqual(write.references, {
    assignment: selected.pin,
    before: { objectId: "result", revision: 2 },
    input: { objectId: "result", revision: 4 },
  });
  const read = JSON.parse(
    JSON.stringify({
      object: {
        id: "execution",
        parentId: "root",
        effectivelyArchived: false,
        contractKey: "secretary/execution",
      },
      revision: {
        revision: 1,
        contractVersion: "1.6.0",
        mediaType: "application/json",
        contentHash: hashJson(value),
        byteLength: Buffer.byteLength(canonical(value)),
        references: write.references,
        createdAt: at,
      },
      content: write.content,
    }),
  );
  assert.deepEqual(store.decode("secretary/execution", read).value, value);
  read.revision.references.input.revision = 3;
  assert.throws(() => store.decode("secretary/execution", read), {
    code: "secretary_evidence_conflict",
  });
  read.revision.references.input.revision = 4;
  delete read.revision.references.before;
  assert.throws(() => store.decode("secretary/execution", read), {
    code: "secretary_evidence_conflict",
  });
});

test("object triggers validate exact selections and escaped JSON pointers", () => {
  assert.doesNotThrow(() => validateAssignment(selected.value));
  for (const change of [
    { objectIds: ["result", "result"] },
    { paths: ["data/activity"] },
    { paths: ["/bad~2escape"] },
  ])
    assert.throws(
      () =>
        validateAssignment({
          ...selected.value,
          trigger: { ...trigger, ...change },
        }),
      { code: "secretary_assignment_invalid" },
    );
  assert.equal(pointer({ "a/b": { "~": 4 } }, "/a~1b/~0"), 4);
  assert.equal(pointer({}, "/constructor"), null);
  assert.deepEqual(
    objectProjection(content(["one"]), trigger),
    objectProjection(content(["one"], "2026-10-07T08:03:00.000Z"), trigger),
  );
});

test("object admission requires a complete observation after the grouping window and within its age", () => {
  const dueAt = "2026-10-07T08:05:00.000Z",
    now = new Date("2026-10-07T08:06:00.000Z");
  assert.equal(
    observationReady(content(["one"], dueAt), trigger, dueAt, now),
    true,
  );
  assert.equal(
    observationReady(content(["one"], dueAt, false), trigger, dueAt, now),
    false,
  );
  assert.equal(
    observationReady(content(["one"], at), trigger, dueAt, now),
    false,
  );
  assert.equal(
    observationReady(
      content(["one"], dueAt),
      trigger,
      dueAt,
      new Date("2026-10-07T08:09:00.000Z"),
    ),
    false,
  );
  assert.equal(
    observationReady(
      content(["one"], "2026-10-07T08:07:00.000Z"),
      trigger,
      dueAt,
      now,
    ),
    false,
  );
});

function fixture() {
  const rows = new Map(),
    revisions = new Map();
  let latest = 0;
  const store = {
    technicalNamed(key, name) {
      return rows.get(key + ":" + name) ?? null;
    },
    technicalList(key) {
      return [...rows.values()].filter((row) => row.key === key);
    },
    technicalCreate(key, name, value) {
      const row = { key, name, pin: { objectId: name, revision: 1 }, value };
      rows.set(key + ":" + name, row);
      return row;
    },
    technicalAmend(key, row, value) {
      const next = {
        ...row,
        pin: { ...row.pin, revision: row.pin.revision + 1 },
        value,
      };
      rows.set(key + ":" + row.name, next);
      return next;
    },
  };
  const engine = {
    store,
    settings: { identity: { principalId: "secretary" } },
    client: {
      async request(method, args) {
        assert.equal(method, "objects.read");
        assert.equal(args.objectId, "result");
        const revision = args.revision ?? latest;
        return {
          content: { encoding: "json", value: revisions.get(revision) },
          revision: { revision },
          object: { effectivelyArchived: false },
        };
      },
    },
  };
  const event = (sequence, revision, occurredAt = at) => ({
    sequence,
    topic: "hive.object.changed",
    topicVersion: "1.0.0",
    source: "principal:collector",
    occurredAt,
    payload: { operation: "write", objectId: "result", revision },
  });
  return {
    engine,
    event,
    write(value) {
      revisions.set(++latest, value);
      return latest;
    },
  };
}

test("object windows ignore poll metadata, cancel read/reverted messages, and recover admission once", async () => {
  const f = fixture();
  let scheduler = new AssignmentScheduler(f.engine),
    queued = [];
  const attach = () => {
    scheduler.enqueue = async (_assignment, event) => {
      if (!queued.some((item) => item.key === event.key)) queued.push(event);
    };
  };
  attach();
  f.write(content([]));
  await scheduler.collectObjectEvent(selected, f.event(1, 1));
  f.write(content(["one"]));
  await scheduler.collectObjectEvent(selected, f.event(2, 2));
  f.write(content([]));
  await scheduler.collectObjectEvent(selected, f.event(3, 3));
  await scheduler.flushDueObjectWindows(
    selected,
    {},
    new Date("2026-10-07T08:06:00.000Z"),
  );
  assert.equal(queued.length, 0);
  f.write(content(["two"]));
  await scheduler.collectObjectEvent(selected, f.event(4, 4));
  // Local technical state survives scheduler recreation; replaying the journal is harmless.
  scheduler = new AssignmentScheduler(f.engine);
  attach();
  await scheduler.collectObjectEvent(selected, f.event(4, 4));
  f.write(content(["two"], "2026-10-07T08:05:10.000Z", false));
  await scheduler.collectObjectEvent(selected, f.event(5, 5));
  await scheduler.flushDueObjectWindows(
    selected,
    {},
    new Date("2026-10-07T08:06:00.000Z"),
  );
  assert.equal(queued.length, 0);
  f.write(content(["two"], "2026-10-07T08:06:00.000Z"));
  await scheduler.collectObjectEvent(selected, f.event(6, 6));
  await scheduler.flushDueObjectWindows(
    selected,
    {},
    new Date("2026-10-07T08:06:00.000Z"),
  );
  await scheduler.flushDueObjectWindows(
    selected,
    {},
    new Date("2026-10-07T08:06:00.000Z"),
  );
  assert.equal(queued.length, 1);
  assert.equal(queued[0].key, "object-batch:4:result");
  assert.deepEqual(queued[0].payload.before, { "/data/activity": [] });
  assert.deepEqual(queued[0].payload.after, { "/data/activity": ["two"] });
  assert.equal(queued[0].payload.revision, 6);
  assert.equal(
    scheduler.eventMatches(selected.value, {
      ...f.event(7, 7),
      source: "principal:secretary",
    }),
    false,
  );
  assert.equal(
    scheduler.eventMatches(selected.value, {
      ...f.event(7, 7),
      payload: { operation: "write", objectId: "other", revision: 7 },
    }),
    false,
  );
});
