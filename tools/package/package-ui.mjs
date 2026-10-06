import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  writeFileSync,
  existsSync,
} from "node:fs";
import { resolve, join, dirname, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { buildIdentity } from "../../packages/host-runtime/src/build-identity.mjs";

const project = fileURLToPath(new URL("../../", import.meta.url));
const source = resolve(project, "packages/ui");
const npm = process.env.npm_execpath;
if (!npm)
  throw new Error(
    "Run through npm run package:ui to use its exact npm executable.",
  );
const files = [];
function walk(directory) {
  if (realpathSync(directory) !== directory)
    throw new Error("UI package source must not contain directory links.");
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isSymbolicLink())
      throw new Error("UI package source must not contain links.");
    if (entry.isDirectory()) walk(path);
    else if (entry.isFile()) files.push(path);
    else throw new Error("UI package source contains a non-file entry.");
  }
}
walk(join(source, "src"));
walk(join(source, "licenses"));
for (const name of ["package.json", "NOTICE.md"])
  files.push(join(source, name));
const paths = files;
const snapshots = new Map(
  paths.map((path) => {
    if (realpathSync(path) !== path)
      throw new Error("Package inputs must be actual files, not links.");
    return [path, readFileSync(path)];
  }),
);
const license = readFileSync(join(project, "LICENSE"));
const manifest = JSON.parse(snapshots.get(join(source, "package.json")));
const { buildId } = buildIdentity(project);
const staging = resolve(project, ".local", "ui-package-" + randomUUID());
mkdirSync(staging, { recursive: true });
if (realpathSync(staging) !== staging)
  throw new Error("UI package staging must be an actual owned directory.");
for (const path of files) {
  const target = join(staging, relative(source, path));
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, snapshots.get(path), { flag: "wx" });
}
writeFileSync(join(staging, "LICENSE"), license, { flag: "wx" });
const output = resolve(project, ".local/packages");
mkdirSync(output, { recursive: true });
if (realpathSync(output) !== output)
  throw new Error("Package output must be an actual owned directory.");
const packed = spawnSync(
  process.execPath,
  [
    npm,
    "pack",
    staging,
    "--pack-destination",
    staging,
    "--json",
    "--ignore-scripts",
  ],
  { cwd: project, encoding: "utf8", timeout: 60_000, windowsHide: true },
);
if (packed.error || packed.status !== 0)
  throw packed.error ?? new Error(packed.stderr);
const result = JSON.parse(packed.stdout)[0],
  bytes = readFileSync(join(staging, result.filename));
const path = join(output, `ivy-ui-${manifest.version}-${buildId.slice(7)}.tgz`);
if (existsSync(path)) throw new Error("Immutable UI package already exists.");
writeFileSync(path, bytes, { flag: "wx" });
const receipt = {
  package: manifest.name,
  version: manifest.version,
  buildId,
  path,
  integrity: result.integrity,
};
writeFileSync(
  join(output, "ui-package.json"),
  JSON.stringify(receipt, null, 2) + "\n",
);
console.log(JSON.stringify(receipt));
