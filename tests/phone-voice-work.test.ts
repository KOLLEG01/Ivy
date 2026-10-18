import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { PhoneJournal } from "../services/phone-bridge/src/runtime/journal.js";
import { PhoneVoiceWorks } from "../services/phone-bridge/src/runtime/voice-work.js";
import type { PhoneCall } from "../services/phone-bridge/src/runtime/admission.js";

function fixture(t: test.TestContext) {
  const root = mkdtempSync(join(tmpdir(), "ivy-phone-work-"));
  const journal = new PhoneJournal(root, {
    hostId: "fixture",
    serviceNodeId: "phone",
  });
  let now = 1_000_000;
  const store = new PhoneVoiceWorks(journal, () => now);
  const call = {
    callId: randomUUID(),
    principalId: "caller",
    policyHash: "policy",
  } as PhoneCall;
  const threadId = randomUUID(),
    turnId = randomUUID();
  store.started(call, threadId, turnId, "runtime", "party");
  t.after(() => {
    journal.close();
    rmSync(root, { recursive: true, force: true });
  });
  return {
    store,
    journal,
    call,
    threadId,
    turnId,
    now: (value: number) => {
      now = value;
    },
    clock: () => now,
  };
}

test("Voice work deadline starts at actual end and keeps running during user input", (t) => {
  const f = fixture(t);
  f.store.detach(f.call.callId, f.clock() - 20_000);
  f.store.waiting(f.threadId);
  assert.equal(f.store.get(f.threadId)!.deadline, 1_280_000);
  f.now(1_279_999);
  assert.equal(f.store.canAnswer(f.store.get(f.threadId)!), true);
  f.now(1_280_000);
  const [claim] = f.store.claimExpired();
  assert.equal(claim!.turnId, f.turnId);
  assert.equal(f.store.canAnswer(f.store.get(f.threadId)!), false);
  assert.equal(f.store.claimExpired()[0]!.interruptId, claim!.interruptId);
  f.store.interrupted(claim!, null);
  assert.equal(
    f.store.get(f.threadId)!.state,
    "halting",
    "acknowledgement is not a completed turn",
  );
  f.store.completed(f.threadId, f.turnId, "interrupted");
  assert.equal(f.store.get(f.threadId)!.state, "paused_deadline");
  assert.equal(f.store.protected(f.threadId), true);
});

test("same authorized caller rejoins once; old disconnect cannot extend or stop a new connection", (t) => {
  const f = fixture(t),
    next = { ...f.call, callId: randomUUID() };
  f.store.detach(f.call.callId, f.clock());
  for (const wrong of [{ principalId: "other" }, { policyHash: "other" }])
    assert.equal(
      f.store.rejoin({ ...next, ...wrong }, "runtime", "party"),
      undefined,
    );
  assert.equal(f.store.rejoin(next, "other", "party"), undefined);
  assert.equal(f.store.rejoin(next, "runtime", "other"), undefined);
  assert.equal(f.store.rejoin(next, "runtime", "party")!.turnId, f.turnId);
  f.now(1_299_999);
  f.store.attach(f.threadId, next);
  f.now(1_300_001);
  assert.deepEqual(f.store.claimExpired(), []);
  assert.equal(f.store.detach(f.call.callId, f.clock()), undefined);
  assert.equal(f.store.halt(f.call.callId), undefined);
  f.store.detach(next.callId, f.clock());
  assert.equal(f.store.get(f.threadId)!.deadline, 1_600_001);
  f.store.detach(next.callId, f.clock() + 20_000);
  assert.equal(f.store.get(f.threadId)!.deadline, 1_600_001);
});

test("deadline and completion races never restart a completed or expired turn", (t) => {
  const f = fixture(t),
    next = { ...f.call, callId: randomUUID() };
  f.store.detach(f.call.callId, f.clock());
  f.now(1_300_000);
  assert.equal(f.store.rejoin(next, "runtime", "party"), undefined);
  assert.throws(() => f.store.attach(f.threadId, next), /reconciliation/);
  f.store.completed(f.threadId, f.turnId, "completed");
  assert.deepEqual(f.store.claimExpired(), []);
  assert.equal(f.store.get(f.threadId)!.state, "completed");
  f.store.started(undefined, f.threadId, randomUUID(), "runtime", "party");
  assert.equal(
    f.store.claimExpired().length,
    1,
    "an unrequested detached turn is stopped immediately",
  );
});

test("recovery retains original detached deadline and grants no fresh allowance to a lost connected call", (t) => {
  const f = fixture(t);
  f.store.detach(f.call.callId, f.clock());
  f.now(1_100_000);
  const recovered = new PhoneVoiceWorks(f.journal, f.clock);
  assert.equal(recovered.get(f.threadId)!.deadline, 1_300_000);
  recovered.attach(f.threadId, { ...f.call, callId: randomUUID() });
  const crashed = new PhoneVoiceWorks(f.journal, f.clock);
  assert.equal(crashed.claimExpired().length, 1);
  assert.equal(crashed.get(f.threadId)!.deadline, f.clock());
});

test("missing end time and unknown interrupt outcomes never enable another answer or automatic rejoin", (t) => {
  const f = fixture(t);
  f.store.detach(f.call.callId, null);
  const [claim] = f.store.claimExpired();
  assert.ok(claim);
  f.store.interrupted(claim!, "native_timeout");
  assert.equal(f.store.get(f.threadId)!.state, "outcome_unknown");
  assert.equal(f.store.protected(f.threadId), true);
  assert.deepEqual(f.store.claimExpired(), []);
  assert.equal(
    f.store.rejoin({ ...f.call, callId: randomUUID() }, "runtime", "party"),
    undefined,
  );
});
