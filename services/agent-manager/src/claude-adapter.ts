import { createHash } from "node:crypto";
import { lstat, readFile, readdir, realpath, stat } from "node:fs/promises";
import { isAbsolute, join } from "node:path";
import { IvyError, requireThat } from "../../../packages/sdk/src/node.js";

/** The adapter is a separately built, pinned artifact; the native catalog binds its bytes. */
export async function checkedClaudeAdapter(
  root: string,
  expectedHash: string,
): Promise<string> {
  requireThat(
    isAbsolute(root),
    "invalid_arguments",
    "Claude adapter root must be absolute.",
  );
  const actualRoot = await realpath(root);
  const source = join(actualRoot, "dist", "src");
  const names = (await readdir(source)).filter((name) => name.endsWith(".mjs"));
  requireThat(
    names.includes("adapter.mjs"),
    "native_build_mismatch",
    "Claude adapter entrypoint is missing.",
  );
  const files = [
    "package-lock.json",
    ...names.map((name) => "dist/src/" + name),
  ].sort();
  const hash = createHash("sha256");
  for (const name of files) {
    const path = join(actualRoot, name);
    const resolved = await realpath(path);
    requireThat(
      resolved === path && (await stat(path)).isFile(),
      "native_build_mismatch",
      "Claude adapter member must be a regular file.",
    );
    hash
      .update(name)
      .update("\0")
      .update(
        createHash("sha256")
          .update(await readFile(path))
          .digest("hex"),
      )
      .update("\0");
  }
  if ("sha256:" + hash.digest("hex") !== expectedHash)
    throw new IvyError(
      "native_build_mismatch",
      "Claude adapter bytes differ from the pinned catalog.",
    );
  return join(source, "adapter.mjs");
}

const paidConfigurationKeys = [
  "ANTHROPIC_API_KEY",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_BASE_URL",
  "CLAUDE_CODE_USE_BEDROCK",
  "CLAUDE_CODE_USE_VERTEX",
  "CLAUDE_CODE_USE_FOUNDRY",
];

/** Check the isolated Claude settings without reading or reporting credential values. */
export async function assertClaudeSubscriptionConfig(
  configRoot: string,
  environment: NodeJS.ProcessEnv = process.env,
): Promise<void> {
  requireThat(
    paidConfigurationKeys.every((key) => !environment[key]),
    "billing_configuration_conflict",
    "Claude subscription mode conflicts with API or cloud gateway environment settings.",
  );
  for (const name of ["settings.json", "settings.local.json"]) {
    const path = join(configRoot, name);
    let bytes: Buffer;
    try {
      const info = await lstat(path);
      requireThat(
        info.isFile() && !info.isSymbolicLink() && info.size <= 1024 * 1024,
        "billing_configuration_conflict",
        "Claude settings must be a bounded regular file.",
      );
      bytes = await readFile(path);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
      throw error;
    }
    let settings: Record<string, unknown>;
    try {
      settings = JSON.parse(bytes.toString("utf8")) as Record<string, unknown>;
    } catch {
      throw new IvyError(
        "billing_configuration_conflict",
        "Claude settings are not valid JSON.",
      );
    }
    requireThat(
      settings !== null &&
        typeof settings === "object" &&
        !Array.isArray(settings),
      "billing_configuration_conflict",
      "Claude settings must contain an object.",
    );
    const env =
      settings.env &&
      typeof settings.env === "object" &&
      !Array.isArray(settings.env)
        ? (settings.env as Record<string, unknown>)
        : {};
    requireThat(
      !paidConfigurationKeys.some((key) => Object.hasOwn(env, key)) &&
        !Object.hasOwn(settings, "apiKeyHelper") &&
        settings.forceLoginMethod !== "console",
      "billing_configuration_conflict",
      "Claude subscription mode conflicts with API or cloud gateway settings.",
    );
  }
}
