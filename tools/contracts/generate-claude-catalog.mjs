import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";

const root = new URL("../../", import.meta.url);
const read = (path) => JSON.parse(readFileSync(new URL(path, root), "utf8"));
const lock = read("services/agent-manager/claude/adapter.lock.json");
const base = read("specs/native/codex-0.158.0/catalog.json");
const patch = readFileSync(
  new URL("services/agent-manager/claude/adapter.patch", root),
);
const hash = (bytes) =>
  "sha256:" + createHash("sha256").update(bytes).digest("hex");
if (hash(patch) !== lock.patchHash)
  throw new Error("Claude adapter patch differs from its lock");

// This is the tested Ivy surface of the pinned 0.142.3 adapter plus one
// adapter-side 0.158 item paging extension. Do not expose upstream stubs.
const methods = [
  "initialize",
  "thread/start",
  "thread/resume",
  "thread/list",
  "thread/read",
  "thread/turns/list",
  "thread/items/list",
  "thread/loaded/list",
  "turn/start",
  "turn/steer",
  "turn/interrupt",
  "model/list",
  "account/rateLimits/read",
];
const requests = [
  "item/commandExecution/requestApproval",
  "item/fileChange/requestApproval",
  "item/tool/requestUserInput",
];
const select = (items, names) =>
  names.map((method) => {
    const value = items.find((item) => item.method === method);
    if (!value) throw new Error("Missing checked protocol method: " + method);
    return value;
  });
const catalog = {
  ...base,
  version: lock.protocolVersion,
  sourceHash: hash(
    JSON.stringify({
      commit: lock.commit,
      patchHash: lock.patchHash,
      bundleHash: lock.bundleHash,
      protocolVersion: lock.protocolVersion,
      schemaSourceHash: base.sourceHash,
      methods,
      requests,
    }),
  ),
  nativeExecutableHash: lock.bundleHash,
  clientRequests: select(base.clientRequests, methods),
  serverRequests: select(base.serverRequests, requests),
  derivedLegacyTypes: [],
};
const target = new URL(
  `specs/native/codex-${lock.protocolVersion}/catalog.json`,
  root,
);
const content = JSON.stringify(catalog) + "\n";
if (process.argv.includes("--check")) {
  if (readFileSync(target, "utf8") !== content)
    throw new Error("Stale Claude adapter catalog");
} else writeFileSync(target, content);
