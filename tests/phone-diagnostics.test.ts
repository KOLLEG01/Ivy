import test from "node:test";
import assert from "node:assert/strict";
import { PhoneDiagnostics } from "../services/phone-bridge/src/runtime/diagnostics.js";
import { validateShared } from "../packages/contracts/src/validation.js";

test("registration warnings survive healthy polling and clear only on observed recovery or disabled registration", () => {
  const diagnostics = new PhoneDiagnostics("phone");
  diagnostics.registrationObserved(true, {
    state: "failed",
    lastKeepAliveError: "private OS detail",
  });
  diagnostics.pollSucceeded();
  assert.deepEqual(
    diagnostics.snapshot([]).map((x) => x.code),
    ["phone_registration_unavailable"],
  );
  diagnostics.registrationObserved(true, {
    state: "registered",
    lastKeepAliveError: "private OS detail",
  });
  const warnings = diagnostics.snapshot([]);
  assert.equal(warnings[0]!.code, "phone_registration_keepalive_failed");
  validateShared("Diagnostic", warnings[0]);
  assert.equal(JSON.stringify(warnings).includes("private OS detail"), false);
  diagnostics.registrationObserved(true, {
    state: "registered",
    lastKeepAliveError: null,
  });
  assert.deepEqual(diagnostics.snapshot([]), []);
  diagnostics.registrationObserved(true, null);
  assert.equal(diagnostics.snapshot([]).length, 1);
  diagnostics.registrationObserved(false, null);
  assert.deepEqual(diagnostics.snapshot([]), []);
});

test("App Tools failure remains visible through healthy SIP polls until a successful probe", () => {
  const diagnostics = new PhoneDiagnostics("phone");
  assert.equal(diagnostics.voiceFailed("phone_voice_unavailable"), true);
  assert.equal(diagnostics.voiceFailed("phone_voice_unavailable"), false);
  diagnostics.pollSucceeded();
  diagnostics.callSucceeded("another-call");
  const warnings = diagnostics.snapshot([]);
  assert.deepEqual(
    warnings.map((item) => item.code),
    ["phone_voice_unavailable"],
  );
  validateShared("Diagnostic", warnings[0]);
  assert.equal(diagnostics.voiceSucceeded(), true);
  assert.deepEqual(diagnostics.snapshot([]), []);
});

test("call diagnostics survive release and healthy polling until a later call connects", () => {
  const diagnostics = new PhoneDiagnostics("phone");
  diagnostics.callFailed("first", "first_unknown");
  diagnostics.callFailed("second", "second_unknown");
  diagnostics.pollSucceeded();
  assert.deepEqual(
    diagnostics.snapshot(["first", "second"]).map((item) => item.code),
    ["first_unknown", "second_unknown"],
  );
  assert.deepEqual(
    diagnostics.snapshot(["second"]).map((item) => item.code),
    ["first_unknown", "second_unknown"],
  );
  assert.deepEqual(
    diagnostics.snapshot([]).map((item) => item.code),
    ["first_unknown", "second_unknown"],
  );
  diagnostics.callSucceeded("recovery");
  assert.deepEqual(diagnostics.snapshot([]), []);
});

test("Phone diagnostics keep uncertain cleanup through healthy polling and release until a later connection", () => {
  let at = "2026-09-09T07:00:00.000Z";
  const diagnostics = new PhoneDiagnostics("phone", () => at);
  diagnostics.callFailed("original-call", "phone_voice_cleanup_unknown");
  diagnostics.pollFailed("native_unavailable");
  const first = diagnostics.snapshot("original-call");
  assert.equal(first.length, 2);
  for (const item of first) validateShared("Diagnostic", item);
  at = "2026-09-09T07:00:01.000Z";
  diagnostics.callFailed("original-call", "phone_voice_cleanup_unknown");
  diagnostics.pollSucceeded();
  const current = diagnostics.snapshot("original-call");
  assert.equal(current.length, 1);
  assert.equal(current[0]!.firstObservedAt, first[0]!.firstObservedAt);
  assert.equal(current[0]!.lastObservedAt, at);
  current[0]!.code = "mutated";
  assert.equal(
    diagnostics.snapshot("original-call")[0]!.code,
    "phone_voice_cleanup_unknown",
  );
  assert.ok(!JSON.stringify(first).includes("original-call"));
  assert.equal(
    diagnostics.snapshot(null)[0]!.code,
    "phone_voice_cleanup_unknown",
  );
  diagnostics.callSucceeded("next-call");
  assert.deepEqual(diagnostics.snapshot(null), []);
});

test("Phone diagnostics bound repeated failures and clear retained failures only after a replacement connects", () => {
  const diagnostics = new PhoneDiagnostics("phone");
  for (let i = 0; i < 100; i++) {
    diagnostics.callFailed("old-call", "phone_voice_cleanup_unknown");
    diagnostics.pollFailed("native_unavailable");
  }
  assert.equal(diagnostics.snapshot("old-call").length, 2);
  assert.deepEqual(
    diagnostics.snapshot("new-call").map((item) => item.code),
    ["phone_voice_cleanup_unknown", "native_unavailable"],
  );
  diagnostics.pollSucceeded();
  assert.deepEqual(
    diagnostics.snapshot("new-call").map((item) => item.code),
    ["phone_voice_cleanup_unknown"],
  );
  diagnostics.callSucceeded("new-call");
  assert.deepEqual(diagnostics.snapshot("new-call"), []);
});
