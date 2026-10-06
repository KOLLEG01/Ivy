import { mkdir, writeFile, readFile, stat, rm } from "node:fs/promises";
import { rmSync } from "node:fs";
import { dirname, join, resolve, relative } from "node:path";
import { createHash } from "node:crypto";
import { startProcess } from "../../../packages/host-runtime/src/process.js";
import { requireThat } from "../../../packages/sdk/src/node.js";
import { outputSchema } from "./schema.js";
import type { Output, Settings, Task } from "./schema.js";

const taskErrors: Record<string, string> = {
  authentication_required: "Renew the configured credentials or token.",
  interaction_required:
    "Complete the provider verification described in the source setup guide.",
  configuration_invalid: "Check the task settings and assigned secrets.",
  provider_unavailable:
    "The provider is unavailable or its response is unsupported.",
};
const taskErrorCodes = Object.keys(taskErrors);
// Kept beside each task's dependencies so normal ESM package resolution works.
const worker = `import {writeFileSync} from 'node:fs';
process.stdin.setEncoding('utf8');
let input=''; for await (const chunk of process.stdin) input+=chunk;
const context=JSON.parse(input); context.signal=AbortSignal.timeout(context.timeoutMs);
try { const {default:run}=await import('./task.mjs');
const output=JSON.stringify(await run(context));
if(typeof output!=='string'||Buffer.byteLength(output)>1000000) process.exit(79);
writeFileSync('result.json',output,{mode:0o600}); process.exit(0);
} catch (error) {
const code=${JSON.stringify(taskErrorCodes)}.indexOf(error?.code);
process.exit(code < 0 ? 1 : 80 + code); }
`;
export class TaskRunner {
  constructor(
    readonly workRoot: string,
    readonly artifactRoot: string,
    readonly settings: Settings,
  ) {}
  clearAuthentication(taskId: string) {
    const root = resolve(this.workRoot),
      directory = resolve(root, taskId);
    requireThat(
      /^[a-z0-9][a-z0-9_-]{0,63}$/.test(taskId) &&
        relative(root, directory) === taskId,
      "invalid_arguments",
      "Invalid task authentication directory.",
    );
    rmSync(join(directory, ".auth"), { recursive: true, force: true });
  }
  private async execute(
    cwd: string,
    args: string[],
    timeoutMs: number,
    signal: AbortSignal,
    input = "",
  ) {
    signal.throwIfAborted();
    const run = await startProcess(
      { executable: "node", args, timeoutMs },
      this.artifactRoot,
      { node: process.execPath },
      {
        executionCwd: cwd,
        environment: {
          HOME: cwd,
          USERPROFILE: cwd,
          CODEX_HOME: join(cwd, "home"),
          NODE_OPTIONS: "",
          NPM_CONFIG_USERCONFIG: join(cwd, ".npmrc"),
          NPM_CONFIG_CACHE: join(cwd, ".npm-cache"),
        },
        maxOutputBytes: 65536,
      },
    );
    let expired = false;
    const stop = () => {
      void run.stop(100);
    };
    const timer = setTimeout(() => {
      expired = true;
      stop();
    }, timeoutMs);
    signal.addEventListener("abort", stop, { once: true });
    if (signal.aborted) stop();
    run.child.stdin!.on("error", () => undefined);
    run.child.stdin!.end(input);
    try {
      const result = await run.completion;
      requireThat(
        result.errorCode !== "outcome_unknown",
        "outcome_unknown",
        "Task process termination could not be confirmed.",
      );
      signal.throwIfAborted();
      requireThat(
        !expired,
        "deadline_exceeded",
        "Task exceeded its time limit.",
      );
      if (input) {
        requireThat(
          result.exitCode !== 79,
          "limit_exceeded",
          "Task result exceeds 1 MB.",
        );
        // Exit codes do not depend on stderr being flushed before process exit.
        const code =
          result.exitCode == null
            ? undefined
            : taskErrorCodes[result.exitCode - 80];
        if (code) requireThat(false, code, taskErrors[code]!);
      }
      requireThat(
        result.exitCode === 0 && !result.errorCode,
        "task_failed",
        "Task process failed. Check script, credentials and provider availability.",
      );
    } finally {
      clearTimeout(timer);
      signal.removeEventListener("abort", stop);
    }
  }
  async run(
    task: Task,
    state: unknown,
    input: unknown,
    signal: AbortSignal,
  ): Promise<Output> {
    const cwd = resolve(this.workRoot, task.id);
    await mkdir(cwd, { recursive: true });
    const dependencies = JSON.stringify(task.dependencies),
      hash = createHash("sha256").update(dependencies).digest("hex");
    let installed = "";
    try {
      installed = await readFile(join(cwd, "dependencies.sha256"), "utf8");
    } catch {
      /* first run */
    }
    if (installed !== hash) {
      await writeFile(
        join(cwd, "package.json"),
        JSON.stringify({
          name: "collector-task",
          private: true,
          type: "module",
          dependencies: task.dependencies,
        }),
      );
      if (Object.keys(task.dependencies).length) {
        const cli =
          this.settings.npmCliPath ??
          join(
            dirname(process.execPath),
            "node_modules",
            "npm",
            "bin",
            "npm-cli.js",
          );
        requireThat(
          (await stat(cli).catch(() => null))?.isFile(),
          "npm_unavailable",
          "Configure npmCliPath for tasks with npm dependencies.",
        );
        await this.execute(
          cwd,
          [
            cli,
            "install",
            "--omit=dev",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--loglevel=error",
          ],
          120000,
          signal,
        );
      } else if (installed)
        await rm(join(cwd, "node_modules"), { recursive: true, force: true });
      await writeFile(join(cwd, "dependencies.sha256"), hash);
    }
    await rm(join(cwd, "result.json"), { force: true });
    await writeFile(join(cwd, "task.mjs"), task.script, { mode: 0o600 });
    await writeFile(join(cwd, "worker.mjs"), worker);
    const secrets: Record<string, string> = {};
    for (const name of task.secretNames) {
      const value = this.settings.secrets[name];
      requireThat(
        value !== undefined,
        "secret_missing",
        "A task secret is not configured: " + name,
      );
      secrets[name] = value;
    }
    const timeoutMs =
      Math.min(task.timeoutSeconds, this.settings.maximumTimeoutSeconds) * 1000;
    await this.execute(
      cwd,
      [
        "--max-old-space-size=" +
          Math.min(task.memoryMb, this.settings.maximumMemoryMb),
        "worker.mjs",
      ],
      timeoutMs,
      signal,
      JSON.stringify({ config: task.config, secrets, state, input, timeoutMs }),
    );
    const resultPath = join(cwd, "result.json");
    requireThat(
      (await stat(resultPath)).size <= 1000000,
      "limit_exceeded",
      "Task result exceeds 1 MB.",
    );
    const output = outputSchema.parse(
      JSON.parse(await readFile(resultPath, "utf8")),
    );
    await rm(resultPath, { force: true });
    return output;
  }
}
