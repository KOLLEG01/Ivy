import test from "node:test";
import assert from "node:assert/strict";
import { dashboardDocument } from "../services/dashboards/src/document.js";
import cases from "./fixtures/dashboards/document.json" with { type: "json" };
import registry from "../services/dashboards/rust/assets/registry.json" with { type: "json" };
import { dashboardRegistry } from "../services/dashboards/src/schema.js";

test("shared dashboard document preserves original bytes and channel escaping", () => {
  for (const fixture of cases)
    assert.equal(
      dashboardDocument(fixture.html, fixture.channel),
      fixture.document,
    );
});

test("embedded Rust registry preserves every TypeScript schema and discovery field", () => {
  assert.deepEqual(registry, dashboardRegistry());
});
