// Differential test oracle only: no Rust service invokes this program.
import { readFileSync } from "node:fs";
import { DashboardRenderer } from "../../services/dashboards/src/render.ts";
const fixtures = JSON.parse(
  readFileSync(new URL("./dashboards/render.json", import.meta.url), "utf8"),
);
const renderer = new DashboardRenderer(process.env.DASHBOARDS_TEST_BROWSER);
try {
  const results = [];
  for (const fixture of fixtures) {
    for (const colorMode of ["color", "grayscale", "monochrome"]) {
      const result = await renderer.render(fixture.html, fixture.data, {
        width: 120,
        height: 100,
        colorMode,
      });
      results.push({ name: fixture.name, colorMode, data: result.data });
    }
  }
  process.stdout.write(JSON.stringify(results));
} finally {
  await renderer.close();
}
