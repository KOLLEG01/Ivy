import { DashboardStore } from "../../services/dashboards/src/store.ts";
import { definition } from "../../services/dashboards/src/schema.ts";
import { IvyError } from "../../packages/contracts/src/errors.ts";
const value = definition.parse({ title: " A ", html: "x", sources: {} });
const item = (v = value) => ({ object: { contractKey: "dashboards/dashboard", parentId: null, effectivelyArchived: false }, revision: { revision: 7, contractVersion: "1.1.0" }, content: { encoding: "json", value: v } });
const rows = [];
const add = (name, action, args, extras = {}) => rows.push({ name, action, args, base: "https://example.test/sub/", node: "node /?😀", root: null, objects: { id: item() }, ...extras });
add("current", "read", ["id"]);
add("old-defaults", "read", ["id"], { objects: { id: { ...item({ title: " A ", html: "x", sources: {} }), revision: { revision: 2, contractVersion: "1.0.0" } } } });
add("missing", "read", ["missing"]);
for (const [key, v] of [["contractKey", "other"], ["parentId", "other"], ["effectivelyArchived", true]]) add(key, "read", ["id"], { objects: { id: { ...item(), object: { ...item().object, [key]: v } } } });
add("version", "read", ["id"], { objects: { id: { ...item(), revision: { revision: 7, contractVersion: "2.0.0" } } } });
add("binary", "read", ["id"], { objects: { id: { ...item(), content: { encoding: "base64", value: "AA==" } } } });
add("malformed-definition", "read", ["id"], { objects: { id: item({ ...value, title: "" }) } });
const token = "a".repeat(32);
for (const supplied of [token, "b".repeat(32), "a".repeat(31), "\ud800"]) add("image-" + supplied.length, "image", ["id", supplied], { objects: { id: item({ ...value, imageToken: token }) } });
add("disabled-image", "image", ["id", token]);
add("create", "save", [value, null, null, "epoch:12:😀/nonce"]);
add("update", "save", [{ ...value, imageToken: token }, "id", 7, "epoch:12:nonce"]);
add("update-missing", "save", [value, "missing", 7, "op"]);
add("write-error", "save", [value, "id", 6, "op"], { writeError: { code: "revision_conflict", message: "stale", outcome: "not_executed" } });
add("lost-write", "save", [value, null, null, "op"], { writeError: { code: "deadline_exceeded", message: "lost", outcome: "unknown" } });
add("delete", "remove", ["id", 7, "op"]);
add("delete-missing-replay", "remove", ["missing", 7, "op"]);
add("delete-wrong-root", "remove", ["id", 7, "op"], { root: "other" });
add("empty-list", "list", ["", 50], { page: { items: [], nextCursor: null } });
const objects = Object.fromEntries(Array.from({ length: 33 }, (_, i) => ["id" + i, item({ ...value, title: "A" + i })]));
add("batch-list", "list", ["cursor", 33], { objects, page: { items: Object.keys(objects).map(objectId => ({ objectId })), nextCursor: "next" } });
add("root-list", "list", [null, 50], { root: "root", page: { items: [], nextCursor: null } });
for (const base of ["https://example.test/sub", "https://example.test/sub/", "https://example.test/a%20b/?old=1#frag"]) add("url-base", "save", [{ ...value, imageToken: token }, null, null, "op"], { base, savedId: "id /?é😀" });
add("surrogate-id", "save", [value, null, null, "op"], { savedId: "\ud800" });
for (const row of rows) {
  row.calls = [];
  const client = { async request(method, params) {
    row.calls.push({ method, params });
    if (method === "objects.query") return row.page;
    if (method === "objects.read") {
      if (!Object.hasOwn(row.objects, params.objectId)) throw new IvyError("not_found", "missing");
      return row.objects[params.objectId];
    }
    if (method === "objects.write") {
      if (row.writeError) throw new IvyError(row.writeError.code, row.writeError.message, row.writeError.outcome);
      return { object: { id: row.savedId ?? "saved" }, revision: { revision: 8 } };
    }
    if (method === "objects.delete") return {};
    throw Error(method);
  } };
  try { row.expected = { value: await new DashboardStore(client, row.root, row.base, row.node)[row.action](...row.args.map(v => v === null ? undefined : v)) }; }
  catch (error) { const e = IvyError.from(error); row.expected = { error: { code: e.code, message: e.message, outcome: e.outcome } }; }
}
process.stdout.write(JSON.stringify(rows));
