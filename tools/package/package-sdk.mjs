import { build } from "esbuild";
import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  cpSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  readdirSync,
} from "node:fs";
import { resolve, join } from "node:path";
import { buildIdentity } from "../../packages/host-runtime/src/build-identity.mjs";
const root = JSON.parse(readFileSync("package.json", "utf8"));
const source = spawnSync("git", ["rev-parse", "HEAD"], {
  encoding: "utf8",
  timeout: 10_000,
  windowsHide: true,
});
if (source.error || source.status !== 0)
  throw source.error ?? new Error(source.stderr);
const sourceCommit = source.stdout.trim();
const destination = resolve(".local/sdk-package-" + randomUUID());
mkdirSync(join(destination, "dist"), { recursive: true });
cpSync("LICENSE", join(destination, "LICENSE"));
// Generate declarations from these exact source inputs. SDK packaging needs no native or UI build.
const typesRoot = join(destination, "declarations");
const types = spawnSync(
  process.execPath,
  [
    "node_modules/typescript/bin/tsc",
    "-p",
    "tsconfig.json",
    "--emitDeclarationOnly",
    "--outDir",
    typesRoot,
  ],
  { stdio: "inherit", timeout: 120_000, windowsHide: true },
);
if (types.error || types.status !== 0)
  throw types.error ?? new Error("SDK declaration generation failed.");
await build({
  entryPoints: Object.fromEntries(
    ["client", "service", "node", "host", "codex-app-tools"].map((name) => [
      name,
      `packages/sdk/src/${name}.ts`,
    ]),
  ),
  outdir: join(destination, "dist"),
  platform: "node",
  target: "node24",
  format: "esm",
  splitting: true,
  bundle: true,
  packages: "external",
  sourcemap: true,
  metafile: true,
  plugins: [
    {
      name: "native-catalog-assets",
      setup(builder) {
        builder.onLoad(
          { filter: /[\\/]checked-native-contract\.ts$/ },
          ({ path }) => ({
            contents: readFileSync(path, "utf8").replaceAll(
              "../../../specs/native/",
              "./specs/native/",
            ),
            loader: "ts",
          }),
        );
      },
    },
  ],
});
const nativeAssets = join(destination, "dist/specs/native");
mkdirSync(nativeAssets, { recursive: true });
for (const entry of readdirSync("specs/native", { withFileTypes: true })) {
  if (!entry.isDirectory()) {
    cpSync(join("specs/native", entry.name), join(nativeAssets, entry.name));
    continue;
  }
  const target = join(nativeAssets, entry.name);
  mkdirSync(target);
  for (const name of readdirSync(join("specs/native", entry.name)))
    if (/^(?:catalog|storage)(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(name))
      cpSync(join("specs/native", entry.name, name), join(target, name));
}
function declarations(source, target) {
  mkdirSync(target, { recursive: true });
  for (const name of readdirSync(source))
    if (name.endsWith(".d.ts")) cpSync(join(source, name), join(target, name));
}
for (const name of ["sdk", "contracts", "host-runtime"])
  declarations(
    join(typesRoot, `packages/${name}/src`),
    join(destination, `types/${name}/src`),
  );
cpSync(
  "packages/host-runtime/src/build-identity.d.mts",
  join(destination, "types/host-runtime/src/build-identity.d.mts"),
);
const npm = process.env.npm_execpath;
if (!npm)
  throw new Error(
    "Run as npm run package:sdk so the exact npm executable is known.",
  );
const exports = Object.fromEntries(
  ["client", "service", "node", "host", "codex-app-tools"].map((name) => [
    "./" + name,
    { types: `./types/sdk/src/${name}.d.ts`, default: `./dist/${name}.js` },
  ]),
);
exports["."] = exports["./client"];
writeFileSync(
  join(destination, "package.json"),
  JSON.stringify(
    {
      name: "@ivy/sdk",
      version: root.version,
      type: "module",
      license: "MIT",
      engines: root.engines,
      exports,
      files: ["dist", "types", "ivy-source.json", "LICENSE"],
      dependencies: Object.fromEntries(
        ["ajv", "re2js", "ws", "zod", "@modelcontextprotocol/client"].map(
          (name) => [name, root.dependencies[name]],
        ),
      ),
    },
    null,
    2,
  ) + "\n",
);
writeFileSync(
  join(destination, "ivy-source.json"),
  JSON.stringify(
    {
      repository: "https://github.com/KOLLEG01/Ivy.git",
      sourceCommit,
      version: root.version,
    },
    null,
    2,
  ) + "\n",
);
const { buildId } = buildIdentity();
mkdirSync(".local/packages", { recursive: true });
const packed = spawnSync(
  process.execPath,
  [
    npm,
    "pack",
    destination,
    "--pack-destination",
    resolve(".local/packages"),
    "--json",
  ],
  { encoding: "utf8", timeout: 60_000 },
);
if (packed.error || packed.status !== 0)
  throw packed.error ?? new Error(packed.stderr);
const result = JSON.parse(packed.stdout)[0];
const path = resolve(".local/packages", result.filename);
const receipt = {
  package: "@ivy/sdk",
  version: root.version,
  buildId,
  sourceCommit,
  path,
  integrity: result.integrity,
};
writeFileSync(
  resolve(".local/packages/sdk-package.json"),
  JSON.stringify(receipt, null, 2) + "\n",
);
console.log(JSON.stringify(receipt));
