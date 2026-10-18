import { DashboardStore } from "../../services/dashboards/src/store.ts";
import { IvyError } from "../../packages/sdk/src/node.ts";
const row = (contractKey, encoding, value, effectivelyArchived = false) => ({
  object: { contractKey, effectivelyArchived }, content: { encoding, value },
});
const cases = [
  { name: "text-and-json", sources: { text: { objectId: "t" }, json: { objectId: "j" } }, objects: { t: row("text", "text", "plain\ud800"), j: row("data", "json", { fresh: true }) } },
  { name: "direct-token-scrub", sources: { d: { objectId: "d" } }, objects: { d: row("dashboards/dashboard", "json", { title: "Dashboard", imageToken: "SECRET", metadata: { nested: true } }) } },
  { name: "contract-first-page-order", sources: { feed: { contractKey: "dashboards/dashboard", limit: 2 } }, objects: { a: row("dashboards/dashboard", "json", { title: "A", imageToken: "SECRET" }), b: row("dashboards/dashboard", "json", { title: "B", imageToken: null }) }, page: { items: [{ objectId: "b" }, { objectId: "a" }], nextCursor: "must-not-follow" } },
  { name: "empty-page", sources: { feed: { contractKey: "empty", limit: 50 } }, objects: {}, page: { items: [], nextCursor: null } },
  { name: "archived-precedence", sources: { x: { objectId: "x" } }, objects: { x: row("ivy/host-configuration", "base64", "", true) } },
  { name: "configuration-forbidden", sources: { x: { objectId: "x" } }, objects: { x: row("ivy/host-configuration", "json", { credential: "SECRET" }) } },
  { name: "binary-rejected", sources: { x: { objectId: "x" } }, objects: { x: row("data", "base64", "AA==") } },
  { name: "missing-fails-snapshot", sources: { x: { objectId: "missing" } }, objects: {} },
];
for (const fixture of cases) {
  const calls = [];
  const client = { async request(method, params) {
    calls.push({ method, params });
    if (method === "objects.query") return fixture.page;
    if (!Object.hasOwn(fixture.objects, params.objectId)) throw new IvyError("not_found", "missing");
    return fixture.objects[params.objectId];
  } };
  try { fixture.expected = { value: await new DashboardStore(client, null, "http://localhost/", "node").data({ sources: fixture.sources }) }; }
  catch (error) { fixture.expected = { error: { code: error.code, message: error.message } }; }
  fixture.calls = calls;
}
process.stdout.write(JSON.stringify(cases));
