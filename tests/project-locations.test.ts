import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  rm,
  readFile,
  writeFile,
  rename,
  symlink,
  realpath,
} from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { ProjectLocations } from "../services/agent-manager/src/projects.js";
import { defaultNativeThreadProject } from "../packages/sdk/src/native-project.js";
import type { Agent } from "../packages/sdk/src/node.js";

test("service tasks retain native membership across nested Linux and extended Windows paths", () => {
  for (const root of ["/home/account/.ivy/codex-projects", "C:\\Users\\Account\\.ivy\\codex-projects", "\\\\server\\share\\internal"]) {
    const inventory: Agent.ProjectsResult = { source: "native", observedAt: new Date().toISOString(),
      defaults: { projectRoot: root, internalProjectRoot: root },
      projects: [{ nativeId: "internal", source: "native", name: "IvyInternal", paths: [root] }] };
    const cwd = root.startsWith("/") ? root + "/secretary/task" : "\\\\?\\" + (root.startsWith("\\\\") ? "UNC\\" + root.slice(2) : root.toUpperCase()) + "\\secretary\\task";
    const params = { cwd, model: "fixture-model" };
    assert.deepEqual(defaultNativeThreadProject(params, inventory), { ...params, projectId: "internal" });
    assert.equal("projectId" in params, false);
    assert.deepEqual(defaultNativeThreadProject({}, inventory), { cwd: root, projectId: "internal" });
    for (const projectId of ["chosen-project", ""]) assert.deepEqual(defaultNativeThreadProject({ ...params, projectId }, inventory), { ...params, projectId });
    assert.deepEqual(defaultNativeThreadProject({ ...params, ephemeral: true }, inventory), { ...params, ephemeral: true });
    assert.throws(() => defaultNativeThreadProject(params, { ...inventory, projects: [] }), { code: "native_internal_project_missing" });
  }
});

test("default service membership respects the closest native project and directory boundaries", () => {
  const inventory: Agent.ProjectsResult = { source: "native", observedAt: new Date().toISOString(),
    defaults: { projectRoot: "/projects", internalProjectRoot: "/internal" }, projects: [
      { nativeId: "internal", source: "native", name: "Internal", paths: ["/internal"] },
      { nativeId: "selected", source: "native", name: "Selected", paths: ["/internal/selected"] },
    ] };
  assert.equal(defaultNativeThreadProject({ cwd: "/internal/selected/task" }, inventory)['projectId'], "selected");
  assert.deepEqual(defaultNativeThreadProject({ cwd: "/internal-other/task" }, inventory), { cwd: "/internal-other/task" });
});

test("project registration returns Codex identities without an Ivy project store", async t => {
  const root = await mkdtemp(join(tmpdir(), "ivy-projects-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const projects = new Map<string, Agent.ProjectSummary>();
  let registrations = 0;
  const register = async ({ name, cwd, idempotencyKey }: { name: string; cwd: string; idempotencyKey: string }) => {
    registrations++;
    const project = projects.get(idempotencyKey) ?? { nativeId: "native-" + projects.size, source: "native" as const, name, paths: [cwd] };
    projects.set(idempotencyKey, project);
    return project;
  };
  const settings = { projectRoot: join(root, "normal"), internalProjectRoot: join(root, "internal") };
  const selection = { kind: "normal", key: "create-project", name: "User work" } as const;
  let locations = new ProjectLocations(join(root, "data"), settings, [], register);
  const first = await locations.resolve(selection, "user");
  locations = new ProjectLocations(join(root, "data"), settings, [], register);
  assert.deepEqual(await locations.resolve(selection, "user"), first);
  assert.equal(projects.size, 1);
  assert.equal(first.project.nativeId, "native-0");
  locations.setNativeProjects([...projects.values()]);
  assert.equal((await locations.resolve({ kind: "existing", cwd: first.cwd }, "user", first.project.nativeId)).project.nativeId, first.project.nativeId);
  const before = registrations;
  locations.setNativeProjects([]);
  await assert.rejects(locations.resolve({ kind: "existing", cwd: first.cwd }, "user", first.project.nativeId), { code: "target_conflict" });
  assert.equal(registrations, before, "Deleted project references must never re-register the project.");
  await assert.rejects(readFile(join(root, "data", "project-backup-paths.json")), { code: "ENOENT" });
});

test("internal and task directories require the native project and reject redirected destinations", async t => {
  const root = await mkdtemp(join(tmpdir(), "ivy-project-boundary-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const internal = join(root, "internal"), protectedRoot = join(root, "protected");
  await mkdir(internal); await mkdir(protectedRoot);
  let registrations = 0;
  const locations = new ProjectLocations(join(root, "data"), { projectRoot: join(root, "normal"), internalProjectRoot: internal }, [protectedRoot],
    async () => { registrations++; throw new Error("Internal projects must not be registered implicitly."); });
  const selection = { kind: "internal", key: "service", name: "Service" } as const;
  await assert.rejects(locations.resolve(selection, "user"), { code: "native_internal_project_missing" });
  locations.setNativeProjects([{ nativeId: "native-internal", source: "native", name: "IvyInternal", paths: [internal] }]);
  const shared = await locations.resolve(selection, "user");
  const task = await locations.resolve({ kind: "task", key: "TASK-0001", name: "Task" }, "user");
  assert.equal(shared.cwd, internal); assert.equal(task.cwd, join(internal, "TASK-0001"));
  assert.equal(shared.project.nativeId, task.project.nativeId); assert.equal(registrations, 0);
  for (const key of ["../escape", "/absolute", "child/name", "child\\name", ".", ".."]) {
    await assert.rejects(locations.resolve({ kind: "task", key, name: "Task" }, "user"), { code: "invalid_arguments" });
  }
  await symlink(protectedRoot, join(internal, "redirect"), process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(locations.resolve({ kind: "task", key: "redirect", name: "Task" }, "user"), { code: "target_conflict" });
});

test("native projects resolve to their current identity across Windows path spelling", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-project-configured-")),
    data = join(root, "data"),
    configured = join(root, "configured");
  await mkdir(data);
  await mkdir(configured);
  const configuredPath =
    process.platform === "win32"
      ? configured.replaceAll("\\", "/")
      : configured;
  const locations = new ProjectLocations(
    data,
    {
      projectRoot: join(root, "normal"),
      internalProjectRoot: join(root, "internal"),
    },
  );
  locations.setNativeProjects([{ nativeId: "configured-project", source: "native", name: "Native", paths: [configuredPath] }]);
  t.after(async () => {
    locations.close();
    await rm(root, { recursive: true, force: true });
  });
  const location = await locations.resolve(
    { kind: "existing", cwd: configuredPath },
    "user",
    "configured-project",
  );
  const canonical = await realpath(configured);
  assert.equal(location.cwd, canonical);
  assert.equal(location.project.nativeId, "configured-project");
  assert.deepEqual(location.project.paths, [configuredPath]);
  await assert.rejects(
    locations.resolve(
      { kind: "existing", cwd: configuredPath },
      "user",
      "different-project",
    ),
    (error: unknown) => (error as { code: string }).code === "target_conflict",
  );
});

test("result-time repository observation distinguishes confirmed and local-only commits", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-project-provenance-")),
    data = join(root, "data"),
    repository = join(root, "repository"),
    remote = join(root, "remote.git");
  await mkdir(data);
  await mkdir(repository);
  const run = promisify(execFile),
    git = (cwd: string, args: string[]) =>
      run("git", ["-C", cwd, ...args], { windowsHide: true });
  await git(root, ["init", "--bare", remote]);
  await git(repository, ["init", "-b", "main"]);
  await git(repository, ["config", "user.email", "ivy@example.invalid"]);
  await git(repository, ["config", "user.name", "Ivy Test"]);
  await writeFile(join(repository, "result.txt"), "published");
  await git(repository, ["add", "."]);
  await git(repository, ["commit", "-m", "published"]);
  await git(repository, ["remote", "add", "origin", remote]);
  await git(repository, ["push", "-u", "origin", "main"]);
  const locations = new ProjectLocations(
    data,
    {
      projectRoot: join(root, "normal"),
      internalProjectRoot: join(root, "internal"),
    },
  );
  locations.setNativeProjects([{ nativeId: "repository", source: "native", name: "Repository", paths: [repository] }]);
  t.after(async () => {
    locations.close();
    await rm(root, { recursive: true, force: true });
  });
  const requirement = {
    kind: "existing_project",
    projectId: "repository",
    path: repository,
    useWorktree: false,
  } as const;
  const confirmed = await locations.workspace(
    "TASK-0001",
    requirement,
    false,
    "host",
    "agent",
    true,
  );
  assert.equal(confirmed.repository?.originState, "confirmed");
  assert.equal(confirmed.repository?.remoteRef, "refs/heads/main");
  await writeFile(join(repository, "result.txt"), "local");
  await git(repository, ["add", "."]);
  await git(repository, ["commit", "-m", "local"]);
  const local = await locations.workspace(
    "TASK-0001",
    requirement,
    false,
    "host",
    "agent",
    true,
  );
  assert.equal(local.repository?.originState, "local_only");
  assert.match(local.repository?.limitation ?? "", /not confirmed reachable/);
});

test("TaskBoard shares the internal root and retains explicitly selected directories", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-task-workspaces-"));
  const data = join(root, "data"), normal = join(root, "normal"), internal = join(root, "internal"), outside = join(root, "outside");
  await Promise.all([mkdir(data), mkdir(outside)]);
  const locations = new ProjectLocations(data, { projectRoot: normal, internalProjectRoot: internal });
  t.after(async () => { locations.close(); await rm(root, { recursive: true, force: true }); });
  locations.setNativeProjects([{ nativeId: "ivy-internal", source: "native", name: "IvyInternal", paths: [internal] }]);

  const plain = await locations.workspace("TASK-0010", { kind: "task_workspace" }, true, "host", "agent");
  assert.equal(plain.canonicalCwd, internal);
  assert.equal(plain.bootstrapPath, internal);
  assert.equal(plain.intendedPath, plain.canonicalCwd);
  assert.equal(plain.nativeProjectId, "ivy-internal");
  assert.equal(await realpath(plain.canonicalCwd), plain.canonicalCwd);
  assert.equal((await locations.workspace("TASK-0020", { kind: "task_workspace" }, true, "host", "agent")).canonicalCwd, internal);
  const service = await locations.resolve({ kind: "internal", key: "secretary", name: "Secretary" }, "secretary");
  assert.equal(service.cwd, internal);
  assert.equal(service.project.nativeId, plain.nativeProjectId);

  const selected = await locations.workspace("TASK-0011", { kind: "directory_path", path: outside }, true, "host", "agent");
  assert.equal(selected.canonicalCwd, outside);
  assert.equal(selected.intendedPath, outside);

  for (const kind of ["repository_path", "new_project_path"] as const) {
    const requirement = kind === "repository_path"
      ? { kind, folderName: "checkout", repositoryUrl: "https://example.invalid/repo.git" }
      : { kind, folderName: "project" };
    const workspace = await locations.workspace(kind === "repository_path" ? "TASK-0012" : "TASK-0013", requirement, true, "host", "agent");
    assert.equal(workspace.canonicalCwd, join(normal, requirement.folderName));
    assert.equal(await realpath(workspace.canonicalCwd), workspace.canonicalCwd);
  }
});

test("TaskBoard creates and reuses a flat worktree as its native working directory", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-task-worktree-"));
  const data = join(root, "data"), source = join(root, "source"), internal = join(root, "internal");
  await Promise.all([mkdir(data), mkdir(source)]);
  const run = promisify(execFile);
  const git = (args: string[]) => run("git", ["-C", source, ...args], { windowsHide: true });
  await git(["init", "-b", "main"]);
  await git(["config", "user.email", "ivy@example.invalid"]);
  await git(["config", "user.name", "Ivy Test"]);
  await writeFile(join(source, "a.txt"), "content");
  await git(["add", "a.txt"]);
  await git(["commit", "-m", "first"]);
  const locations = new ProjectLocations(data, { projectRoot: join(root, "normal"), internalProjectRoot: internal });
  locations.setNativeProjects([{ nativeId: "source", source: "native", name: "Source", paths: [source] }]);
  t.after(async () => { locations.close(); await rm(root, { recursive: true, force: true }); });
  const requirement = { kind: "existing_project", projectId: "source", path: source, useWorktree: true } as const;
  const first = await locations.workspace("TASK-0014", requirement, true, "host", "agent");
  assert.equal(first.canonicalCwd, join(internal, "taskboard-worktree-TASK-0014"));
  assert.equal(first.intendedPath, first.canonicalCwd);
  assert.equal(first.nativeProjectId, "source", "Worktrees retain their source project membership.");
  assert.equal((await run("git", ["-C", first.canonicalCwd, "rev-parse", "--show-toplevel"], { windowsHide: true })).stdout.trim().replaceAll("\\", "/"), first.canonicalCwd.replaceAll("\\", "/"));
  const second = await locations.workspace("TASK-0014", requirement, true, "host", "agent");
  assert.equal(second.canonicalCwd, first.canonicalCwd);
});
