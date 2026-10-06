import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  ownedProcessStops,
  ownedProcessTreeStopped,
  retainOwnedProcessStop,
} from "../services/agent-manager/src/process-evidence.js";

test("owned native process stop proofs survive restart without duplicate epochs", async t => {
  const root = mkdtempSync(join(tmpdir(), "ivy-process-stop-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  assert.deepEqual(await ownedProcessStops(root), []);
  await retainOwnedProcessStop(root, "original-epoch");
  await retainOwnedProcessStop(root, "original-epoch");
  assert.deepEqual((await ownedProcessStops(root)).map(value => value.epoch), ["original-epoch"]);
  assert.equal(await ownedProcessTreeStopped(undefined), false);
});
