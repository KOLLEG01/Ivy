import { cachedBuild } from "./build-cache.mjs";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";
import {
  cpSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Every UI page embeds the Ivy symbol, so browsers never request a missing /favicon.ico.
// currentColor has no meaning in a favicon; the icon follows the browser color scheme itself.
function ivyIcon(project) {
  const path = readFileSync(
    resolve(project, "docs/logo/ivy-symbol.svg"),
    "utf8",
  ).match(/<path d="([^"]+)"/)[1];
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128"><style>path{fill:#111}@media(prefers-color-scheme:dark){path{fill:#fff}}</style><path d="${path}"/></svg>`;
  const link = `<link rel="icon" type="image/svg+xml" href="data:image/svg+xml,${encodeURIComponent(svg)}">`;
  return {
    name: "ivy-icon",
    transformIndexHtml: (html) =>
      html
        .replace(/<link rel="icon"[^>]*>/g, "")
        .replace("</head>", link + "</head>"),
  };
}

export async function buildWeb({ uis = null } = {}) {
  const project = fileURLToPath(new URL("../../", import.meta.url)),
    distribution = resolve(project, "dist");
  const requested = uis ? new Set(uis) : null;
  mkdirSync(distribution, { recursive: true });
  if (realpathSync(distribution) !== distribution)
    throw new Error("Web output must stay in the actual project distribution.");
  const destination = resolve(distribution, "console");
  if (dirname(destination) !== distribution)
    throw new Error("Unexpected Console build destination.");
  try {
    if (lstatSync(destination).isSymbolicLink())
      throw new Error("Console output cannot be a link.");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const common = [
    "LICENSE",
    "package.json",
    "package-lock.json",
    "tsconfig.web.json",
    "packages/ui",
    "packages/sdk",
    "packages/contracts",
    "specs",
    "packages/ui-client/src",
    "tools/build/build-web.mjs",
    "tools/build/build-cache.mjs",
    "docs/logo/ivy-symbol.svg",
  ].map((path) => resolve(project, path));
  // Only values that can change emitted browser bytes belong to the cache identity.
  // Host paths, test settings and unrelated credentials must not invalidate UI builds.
  const context = Object.fromEntries(
    Object.entries(process.env)
      .filter(([key]) => key === "NODE_ENV" || key.startsWith("VITE_"))
      .sort(),
  );
  const cached = (id, inputs, output, build) =>
    cachedBuild(
      {
        stamp: resolve(distribution, ".build-cache", id + ".json"),
        inputs: [...common, ...inputs],
        outputs: [output],
        context,
        force: process.argv.includes("--force"),
      },
      build,
    );
  if (!requested || requested.has("console"))
    await cached(
      "console",
      [resolve(project, "services/hive/console")],
      destination,
      async () => {
        await build({
          configFile: false,
          root: resolve(project, "services/hive/console"),
          base: "./",
          plugins: [vue(), tailwindcss(), ivyIcon(project)],
          build: {
            outDir: destination,
            emptyOutDir: true,
            target: "es2023",
            sourcemap: false,
            chunkSizeWarningLimit: 600,
            assetsDir: "assets",
          },
        });
        // Hive serves this document at its root while static files live under the fixed Console prefix.
        // Relative module imports inside emitted chunks remain relative to their own asset directory.
        const index = resolve(destination, "index.html");
        writeFileSync(
          index,
          readFileSync(index, "utf8").replaceAll(
            '="./assets/',
            '="./console/assets/',
          ),
        );
        cpSync(
          resolve(project, "packages/ui/licenses"),
          resolve(destination, "licenses"),
          { recursive: true },
        );
        cpSync(
          resolve(project, "packages/ui/NOTICE.md"),
          resolve(destination, "NOTICE.md"),
        );
        cpSync(resolve(project, "LICENSE"), resolve(destination, "LICENSE"));
      },
    );
  const appRoot = resolve(distribution, "apps");
  mkdirSync(appRoot, { recursive: true });
  if (realpathSync(appRoot) !== appRoot)
    throw new Error("App output must remain inside the actual distribution.");
  // Incremental builds must not keep the retired Chat UI publishable in a reused distribution.
  rmSync(resolve(appRoot, "chat-ui"), { recursive: true, force: true });
  rmSync(resolve(distribution, ".build-cache", "chat-ui.json"), {
    force: true,
  });
  for (const uiId of ["wiki-ui", "agent-ui", "task-board-ui", "secretary-ui", "dashboards-ui", "data-collector-ui"]) {
    if (requested && !requested.has(uiId)) continue;
    const output = resolve(appRoot, uiId);
    try {
      if (lstatSync(output).isSymbolicLink())
        throw new Error("UI output cannot be a link.");
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    const inputs = [resolve(project, "ui", uiId)];
    if (uiId === "dashboards-ui")
      inputs.push(resolve(project, "services/dashboards/src/document.ts"));
    await cached(uiId, inputs, output, async () => {
      await build({
        configFile: false,
        root: resolve(project, "ui", uiId),
        base: "./",
        plugins: [vue(), tailwindcss(), ivyIcon(project)],
        build: {
          outDir: output,
          emptyOutDir: true,
          target: "es2023",
          sourcemap: false,
          chunkSizeWarningLimit: 600,
          assetsDir: "assets",
          // Static assets are limited to 1 MiB of text, so the editor stack is split.
          rollupOptions: {
            output: {
              manualChunks(id) {
                if (id.includes("node_modules/@milkdown/crepe"))
                  return "editor-ui";
                if (id.includes("node_modules/@milkdown"))
                  return "editor-core";
                if (id.includes("node_modules/prosemirror"))
                  return "editor-engine";
                if (id.includes("node_modules/katex")) return "math";
              },
            },
          },
        },
      });
      cpSync(
        resolve(project, "packages/ui/licenses"),
        resolve(output, "licenses"),
        { recursive: true },
      );
      cpSync(
        resolve(project, "packages/ui/NOTICE.md"),
        resolve(output, "NOTICE.md"),
      );
      cpSync(resolve(project, "LICENSE"), resolve(output, "LICENSE"));
      cpSync(
        resolve(project, "ui", uiId, "ui.json"),
        resolve(output, "ivy-ui.json"),
      );
    });
  }
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const index = process.argv.indexOf("--ui");
  await buildWeb({
    uis:
      index >= 0 && process.argv[index + 1] ? process.argv[index + 1].split(',') : null,
  });
}
