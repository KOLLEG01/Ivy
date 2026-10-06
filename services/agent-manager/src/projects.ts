import { mkdir, realpath, stat, access } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { constants } from "node:fs";
import { basename, isAbsolute, join, resolve } from "node:path";
import { inside } from "../../../packages/host-runtime/src/config.js";
import { accountHome, resolvedFuturePath } from "../../../packages/host-runtime/src/layout.js";
import { hashJson, requireThat } from "../../../packages/sdk/src/node.js";
import type { Agent } from "../../../packages/sdk/src/node.js";

export type RegisterProject = (input: { name: string; cwd: string; idempotencyKey: string }) => Promise<Agent.ProjectSummary>;

/** Codex owns project identities. This resolver only prepares and validates working directories. */
export class ProjectLocations {
  private readonly protectedRoots: string[];
  private nativeProjects: Agent.ProjectSummary[] = [];
  readonly defaults: NonNullable<Agent.ProjectsResult["defaults"]>;
  constructor(
    dataRoot: string,
    settings: Pick<Agent.Settings, "projectRoot" | "internalProjectRoot">,
    protectedRoots: string[] = [],
    private readonly register?: RegisterProject,
  ) {
    this.protectedRoots = [dataRoot, ...protectedRoots];
    this.defaults = {
      projectRoot: settings.projectRoot ?? join(accountHome(), "projects"),
      internalProjectRoot: settings.internalProjectRoot ?? join(accountHome(), ".ivy", "codex-projects"),
    };
    requireThat(Object.values(this.defaults).every(isAbsolute), "invalid_arguments", "Project defaults must be absolute directories on the target host.");
  }
  close(): void { this.nativeProjects = []; }
  setNativeProjects(projects: Agent.ProjectSummary[]): void {
    this.nativeProjects = structuredClone(projects);
  }
  private samePath(left: string, right: string): boolean {
    return process.platform === "win32" ? resolve(left).toLowerCase() === resolve(right).toLowerCase() : resolve(left) === resolve(right);
  }
  private async prepareDirectory(path: string, root: string): Promise<string> {
    const boundaries = await Promise.all(this.protectedRoots.map(resolvedFuturePath));
    const intended = await resolvedFuturePath(path), parent = await resolvedFuturePath(root);
    requireThat(inside(parent, intended) && boundaries.every(value => !inside(value, intended)),
      "target_conflict", "Working directories cannot enter protected service, Codex or artifact storage.");
    await mkdir(path, { recursive: true });
    const cwd = await realpath(path);
    requireThat(this.samePath(cwd, intended) && inside(await realpath(root), cwd) && boundaries.every(value => !inside(value, cwd)),
      "target_conflict", "The working directory was redirected outside its allocation.");
    await access(cwd, constants.R_OK | constants.W_OK);
    return cwd;
  }
  async resolve(selection: Agent.ProjectSelection, principal: string, expectedProjectId?: string): Promise<Agent.ProjectLocation> {
    if (selection.kind === "internal" || selection.kind === "task") {
      const root = this.defaults.internalProjectRoot;
      const project = this.nativeProjects.find(value => value.paths.some(path => this.samePath(path, root)));
      requireThat(project && (!expectedProjectId || project.nativeId === expectedProjectId),
        "native_internal_project_missing", "Register the internal working directory as a Codex project before starting service tasks.");
      if (selection.kind === "task") requireThat(/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(selection.key) && ![".", ".."].includes(selection.key),
        "invalid_arguments", "Task directory key must be one safe relative directory name.");
      const path = selection.kind === "task" ? join(root, selection.key) : root;
      const cwd = await this.prepareDirectory(path, root);
      return { kind: selection.kind, cwd, project: structuredClone(project) };
    }
    let cwd: string;
    if (selection.kind === "existing") {
      requireThat(isAbsolute(selection.cwd), "invalid_arguments", "Selected project cwd must be absolute.");
      cwd = await realpath(selection.cwd);
      requireThat((await stat(cwd)).isDirectory(), "target_conflict", "Selected project cwd must be a directory.");
      await access(cwd, constants.R_OK | constants.W_OK);
    } else {
      const slug = selection.name.replace(/[^a-zA-Z0-9_-]+/g, "-").slice(0, 60).replace(/^-+|-+$/g, "") || "project";
      const root = this.defaults.projectRoot;
      const path = join(root, slug + "-" + hashJson({ principal, key: selection.key }).slice(7, 23));
      requireThat(!inside(await resolvedFuturePath(this.defaults.internalProjectRoot), await resolvedFuturePath(path)),
        "target_conflict", "Normal projects cannot be allocated inside the internal working directory.");
      cwd = await this.prepareDirectory(path, root);
    }
    const existing = this.nativeProjects.find(project => (!expectedProjectId || project.nativeId === expectedProjectId) &&
      project.paths.some(path => this.samePath(path, cwd)));
    requireThat(!expectedProjectId || existing, "target_conflict", "The selected Codex project no longer belongs to this working directory.");
    if (existing) return { kind: selection.kind, cwd, project: structuredClone(existing) };
    requireThat(this.register, "native_inventory_unavailable", "Native project registration is unavailable.");
    const project = await this.register({ name: selection.kind === "existing" ? basename(cwd) || cwd : selection.name, cwd,
      idempotencyKey: hashJson({ principal, selection }) });
    requireThat(project.paths.some(path => this.samePath(path, cwd)), "target_conflict",
      "The original Codex project registration belongs to a different working directory.");
    return { kind: selection.kind, cwd, project };
  }
  async workspace(
    taskKey: string,
    requirement: Agent.WorkspaceRequirement,
    prepare: boolean,
    hostId: string,
    serviceNodeId: string,
    verifyOrigin = false,
  ): Promise<Agent.WorkspaceResolution> {
    requireThat(
      /^TASK-[0-9]{4,}$/.test(taskKey),
      "invalid_arguments",
      "Task workspace requires its immutable Task key.",
    );
    const taskRoot = await resolvedFuturePath(this.defaults.internalProjectRoot),
      bootstrapPath = requirement.kind === "task_workspace" ? taskRoot : resolve(taskRoot, `taskboard-${taskKey}`);
    requireThat(
      inside(taskRoot, bootstrapPath),
      "target_conflict",
      "Task workspace left the configured task root.",
    );
    let intendedPath = bootstrapPath,
      sourcePath: string | null = null,
      project: Agent.WorkspaceResolution["project"] = null,
      useWorktree = false;
    if (requirement.kind === "existing_project") {
      const known = this.nativeProjects.find(
        (value) => value.nativeId === requirement.projectId,
      );
      requireThat(
        known,
        "not_found",
        "Requested Codex project is unavailable on this host.",
      );
      const selected = requirement.path ?? known.paths[0];
      requireThat(
        selected &&
          known.paths.some((value) =>
            process.platform === "win32"
              ? resolve(value).toLowerCase() === resolve(selected).toLowerCase()
              : resolve(value) === resolve(selected),
          ),
        "target_conflict",
        "Requested project path does not belong to the selected project.",
      );
      sourcePath = await realpath(selected);
      await access(sourcePath, constants.R_OK | constants.W_OK);
      useWorktree = requirement.useWorktree;
      intendedPath = useWorktree
        ? resolve(taskRoot, `taskboard-worktree-${taskKey}`)
        : sourcePath;
      project = {
        namespace: "codex",
        kind: "project",
        serviceNodeId,
        nativeId: known.nativeId,
      };
    } else if (
      requirement.kind === "repository_path" ||
      requirement.kind === "new_project_path"
    ) {
      requireThat(
        !requirement.folderName.includes("/") &&
          !requirement.folderName.includes("\\") &&
          ![".", ".."].includes(requirement.folderName),
        "invalid_arguments",
        "Project folder must be one relative directory name.",
      );
      const projectRoot = await resolvedFuturePath(this.defaults.projectRoot);
      intendedPath = resolve(projectRoot, requirement.folderName);
      requireThat(
        inside(projectRoot, intendedPath),
        "target_conflict",
        "Requested project path left projectRoot.",
      );
    } else if (requirement.kind === "directory_path") {
      requireThat(isAbsolute(requirement.path), "invalid_arguments", "Selected directory must be absolute.");
      intendedPath = await realpath(requirement.path);
      requireThat((await stat(intendedPath)).isDirectory(), "target_conflict", "Selected workspace must be a directory.");
      await access(intendedPath, constants.R_OK | constants.W_OK);
    }
    if (prepare) {
      if (requirement.kind === "task_workspace") {
        await this.resolve(
          { kind: "internal", key: "taskboard", name: "IvyInternal" },
          "task-board",
        );
      } else if (useWorktree && sourcePath) {
        await mkdir(taskRoot, { recursive: true });
        const actualRoot = await realpath(taskRoot);
        requireThat(inside(actualRoot, await resolvedFuturePath(intendedPath)), "target_conflict", "Worktree path left internalProjectRoot.");
        const run = promisify(execFile);
        const git = async (path: string, args: string[]) =>
          (await run("git", ["-C", path, ...args], { windowsHide: true, timeout: 30000, maxBuffer: 1024 * 1024 })).stdout.trim();
        const common = async (path: string) => realpath(resolve(path, await git(path, ["rev-parse", "--git-common-dir"])));
        let present = false;
        try { present = (await stat(intendedPath)).isDirectory(); }
        catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
        if (!present) {
          await common(sourcePath);
          await git(sourcePath, ["worktree", "add", "--detach", intendedPath, "HEAD"]);
        }
        requireThat((await realpath(intendedPath)) === intendedPath &&
          (await common(sourcePath)) === (await common(intendedPath)),
          "target_conflict", "Dedicated worktree does not belong to the selected repository.");
      } else if (requirement.kind === "repository_path" || requirement.kind === "new_project_path") {
        await mkdir(this.defaults.projectRoot, { recursive: true });
        const actualRoot = await realpath(this.defaults.projectRoot);
        requireThat(inside(actualRoot, await resolvedFuturePath(intendedPath)), "target_conflict", "Project path left projectRoot.");
        await mkdir(intendedPath, { recursive: true });
        requireThat((await realpath(intendedPath)) === intendedPath,
          "target_conflict", "Requested project path was redirected.");
      }
    }
    const canonicalCwd = intendedPath;
    const inspect = async (
      path: string,
    ): Promise<Agent.RepositoryObservation | null> => {
      try {
        if (!(await stat(path)).isDirectory()) return null;
      } catch {
        return null;
      }
      const run = promisify(execFile),
        git = async (args: string[]) =>
          (
            await run("git", ["-C", path, ...args], {
              windowsHide: true,
              timeout: 3000,
              maxBuffer: 1024 * 1024,
            })
          ).stdout.trim();
      try {
        const commit = await git(["rev-parse", "HEAD"]),
          branch = await git([
            "symbolic-ref",
            "--quiet",
            "--short",
            "HEAD",
          ]).catch(() => ""),
          dirty = (await git(["status", "--porcelain"])).length > 0;
        const rawOriginUrl = await git(["remote", "get-url", "origin"]).catch(
          () => "",
        );
        let originUrl = rawOriginUrl;
        try {
          const parsed = new URL(rawOriginUrl);
          parsed.username = "";
          parsed.password = "";
          originUrl = parsed.toString();
        } catch {
          /* SCP-style remotes have no URL credentials to remove. */
        }
        let originState: Agent.RepositoryObservation["originState"] = originUrl
          ? "unknown"
          : "no_origin";
        let remoteRef: string | null = null,
          limitation: string | null = null;
        if (originUrl && verifyOrigin) {
          remoteRef = branch ? `refs/heads/${branch}` : "HEAD";
          try {
            const line = (await git(["ls-remote", "origin", remoteRef]))
              .split(/\r?\n/)
              .find(Boolean);
            const remoteCommit = line?.split(/\s+/)[0] ?? null;
            if (!remoteCommit) {
              originState = "local_only";
              limitation = `Origin has no ${remoteRef}.`;
            } else if (remoteCommit === commit) originState = "confirmed";
            else {
              const reachable = await git([
                "merge-base",
                "--is-ancestor",
                commit,
                remoteCommit,
              ])
                .then(() => true)
                .catch(() => false);
              originState = reachable ? "confirmed" : "local_only";
              if (!reachable)
                limitation = `Commit is not confirmed reachable from origin ${remoteRef}.`;
            }
          } catch {
            limitation = `Origin ${remoteRef} could not be observed at result publication time.`;
          }
        } else if (originUrl)
          limitation =
            "Origin reachability was not checked for this workspace observation.";
        return {
          name: path.split(/[\\/]/).pop() || path,
          branch: branch || null,
          commit: commit || null,
          dirty,
          originName: originUrl ? "origin" : null,
          originUrl: originUrl || null,
          originState,
          remoteRef,
          observedAt: new Date().toISOString(),
          limitation,
        };
      } catch {
        return null;
      }
    };
    let repository = await inspect(
      useWorktree && verifyOrigin ? intendedPath : (sourcePath ?? intendedPath),
    );
    if (!repository && useWorktree && verifyOrigin && sourcePath) {
      repository = await inspect(sourcePath);
      if (repository)
        repository.limitation = [
          "The intended dedicated worktree had no Git repository; the selected source repository was observed instead.",
          repository.limitation,
        ]
          .filter(Boolean)
          .join(" ");
    }
    const nativeProjectId = project?.nativeId ?? this.nativeProjects
      .flatMap(value => value.paths.map(path => ({ id: value.nativeId, path: resolve(path) })))
      .filter(value => inside(value.path, canonicalCwd))
      .sort((a, b) => b.path.length - a.path.length)[0]?.id;
    if (nativeProjectId) project = { namespace: "codex", kind: "project", serviceNodeId, nativeId: nativeProjectId };
    return {
      hostId,
      serviceNodeId,
      canonicalCwd,
      taskRoot,
      bootstrapPath,
      intendedPath,
      sourcePath,
      project,
      ...(nativeProjectId ? { nativeProjectId } : {}),
      useWorktree,
      repository,
    };
  }
}
