import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { resolve, join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = resolve(import.meta.dirname, "../..");
const lock = JSON.parse(
  readFileSync(
    join(root, "services/agent-manager/claude/adapter.lock.json"),
    "utf8",
  ),
);
const patch = join(root, "services/agent-manager/claude/adapter.patch");
const sha = (data) =>
  "sha256:" + createHash("sha256").update(data).digest("hex");

export function claudeAdapterBundleHash(directory) {
  const files = [
    "package-lock.json",
    ...readdirSync(join(directory, "dist/src"))
      .filter((name) => name.endsWith(".mjs"))
      .map((name) => "dist/src/" + name),
  ].sort();
  const hash = createHash("sha256");
  for (const name of files) {
    const file = join(directory, name);
    if (!statSync(file).isFile())
      throw new Error("Adapter bundle member is not a regular file: " + name);
    hash
      .update(name)
      .update("\0")
      .update(createHash("sha256").update(readFileSync(file)).digest("hex"))
      .update("\0");
  }
  return "sha256:" + hash.digest("hex");
}

function run(executable, args, cwd) {
  const result = spawnSync(executable, args, {
    cwd,
    stdio: "inherit",
    windowsHide: true,
    shell: process.platform === "win32" && executable === "npm",
  });
  if (result.status !== 0)
    throw new Error(
      `${executable} failed (${result.status ?? result.error?.code})`,
    );
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const index = process.argv.indexOf("--out");
  if (index < 0 || !process.argv[index + 1])
    throw new Error("Use --out with a new absolute directory");
  const target = resolve(process.argv[index + 1]);
  if (existsSync(target))
    throw new Error("Output directory already exists; select a new directory");
  if (sha(readFileSync(patch)) !== lock.patchHash)
    throw new Error("Pinned adapter patch differs");
  run("git", ["clone", "--no-checkout", lock.repository, target], root);
  run("git", ["checkout", "--detach", lock.commit], target);
  const revision = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: target,
    encoding: "utf8",
  });
  if (revision.status !== 0 || revision.stdout.trim() !== lock.commit)
    throw new Error("Adapter revision differs");
  run("git", ["apply", "--check", patch], target);
  run("git", ["apply", patch], target);
  run("npm", ["ci", "--ignore-scripts"], target);
  const packageLock = JSON.parse(
    readFileSync(join(target, "package-lock.json"), "utf8"),
  );
  if (
    packageLock.packages["node_modules/@anthropic-ai/claude-agent-sdk"]
      ?.version !== lock.agentSdkVersion
  )
    throw new Error("Agent SDK lock differs");
  run("npm", ["run", "build"], target);
  if (claudeAdapterBundleHash(target) !== lock.bundleHash)
    throw new Error("Adapter bundle differs from checked catalog");
  process.stdout.write(`Pinned Claude adapter built at ${target}\n`);
}
