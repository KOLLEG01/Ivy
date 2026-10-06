import assert from "node:assert/strict";
import { test } from "node:test";
import type { TestContext } from "node:test";
import { mkdtemp, mkdir, readFile, writeFile, rm, rename } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import {
  McpConfigurationManager,
  SkillsManager,
  environmentName,
} from "../services/agent-manager/src/environment.js";
import { loadEnvironmentDefaults } from "../services/agent-manager/src/environment-defaults.js";
import { digest } from "../packages/contracts/src/canonical.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import type { Agent } from "../packages/contracts/src/generated.js";
import type { ServiceConnection } from "../packages/sdk/src/service.js";

type Document = Agent.McpDocument | Agent.SkillsDocument;
async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ivy-agent-environment-")),
    home = join(root, "codex"),
    skillsRoot = join(root, "account", ".agents", "skills");
  await mkdir(home, { recursive: true });
  await mkdir(skillsRoot, { recursive: true });
  t.after(async () => {
    await rm(root, { recursive: true, force: true });
  });
  const documents = new Map<
    string,
    { document: Document; revision: number; contractKey: string }
  >();
  const connection = {
    request: (async (method: string, params: Record<string, unknown>) => {
      const key = String(params["path"] ?? params["objectId"]).replace(
          /^\//,
          "",
        ),
        value = documents.get(key);
      if (!value) throw new IvyError("not_found", "Absent fixture document.");
      const object = {
        id: key,
        currentRevision: value.revision,
        effectivelyArchived: false,
        contractKey: value.contractKey,
      };
      if (method === "objects.stat") return object;
      return {
        object,
        revision: {
          revision: value.revision,
          contractVersion: "1.0.0",
          contentHash: digest(JSON.stringify(value.document)),
        },
        content: { encoding: "json", value: value.document },
      };
    }) as ServiceConnection["request"],
  };
  const put = (kind: "mcp" | "skills", document: Document) =>
    documents.set(environmentName(kind, document.hostId), {
      document,
      revision: 1,
      contractKey: kind === "mcp" ? "agent/mcp-configuration" : "agent/skills",
    });
  return { root, home, skillsRoot, connection, put };
}

test("AgentManager patches an isolated MCP block with its local key and preserves unrelated Codex configuration", async (t) => {
  const f = await fixture(t),
    config = join(f.home, "config.toml"),
    credential = "synthetic-manager-secret";
  await writeFile(
    config,
    'model = "unrelated"\n\n[apps.existing]\nenabled = true\n\n[mcp_servers.existing]\ncommand = "existing"\n',
  );
  const defaults: Agent.McpDocument = {
    schemaVersion: 1,
    hostId: null,
    enabled: true,
    servers: [
      {
        name: "ivy",
        url: "https://hive.example/mcp",
        enabled: true,
        authentication: "agent-manager",
        startupTimeoutSeconds: 15,
        toolTimeoutSeconds: 45,
      },
    ],
  };
  const manager = new McpConfigurationManager(
    f.home,
    "host-a",
    "https://hive.example",
    credential,
    defaults,
  );
  await manager.bootstrap();
  assert.equal(manager.status.state, "applied");
  const first = await readFile(config, "utf8");
  assert.match(first, /# >>> Ivy AgentManager Codex configuration/);
  assert.equal((first.match(/# >>> Ivy AgentManager/g) ?? []).length, 1);
  assert.ok(first.indexOf('model = "unrelated"') < first.indexOf('# >>> Ivy AgentManager'));
  assert.ok(first.indexOf('# <<< Ivy AgentManager') < first.indexOf('[apps.existing]'));
  assert.match(first, /approval_policy = "never"/);
  assert.match(first, /default_permissions = ":danger-full-access"/);
  assert.match(first, /model = "unrelated"/);
  assert.match(first, /\[apps\.existing\]\nenabled = true/);
  assert.match(
    first,
    /\[apps\."asdk_app_6aaab03ab3f48191a8e0e4dbdc434e5d"\]\nenabled = false/,
  );
  assert.match(first, /mcp_servers\."ivy"/);
  assert.match(first, /mcp_servers\.existing/);
  assert.ok(first.includes("Bearer " + credential));
  assert.equal(JSON.stringify(defaults).includes(credential), false);
  f.put("mcp", {
    schemaVersion: 1,
    hostId: null,
    enabled: true,
    servers: [
      {
        name: "public_docs",
        url: "https://example.org/mcp",
        enabled: true,
        authentication: "none",
        startupTimeoutSeconds: 10,
        toolTimeoutSeconds: 20,
      },
    ],
  });
  await manager.synchronize(f.connection);
  const second = await readFile(config, "utf8");
  assert.match(second, /mcp_servers\."public_docs"/);
  assert.doesNotMatch(second, /Bearer/);
  assert.match(second, /mcp_servers\.existing/);
  await writeFile(
    config,
    second.replace("tool_timeout_sec = 20", "tool_timeout_sec = 21"),
  );
  await manager.synchronize(f.connection);
  assert.equal(manager.status.state, "conflict");
  assert.match(await readFile(config, "utf8"), /tool_timeout_sec = 21/);
});

test("AgentManager enables ivy_dev by default and allows a host override to disable it", async (t) => {
  const f = await fixture(t), config = join(f.home, "config.toml");
  const defaults = await loadEnvironmentDefaults('', 'https://hive.example');
  assert.deepEqual(defaults.mcp.servers.map(server => [server.name, server.url, server.enabled]), [
    ['ivy', 'https://hive.example/mcp', true],
    ['ivy_dev', 'https://hive.example/mcp-dev', true],
  ]);
  const manager = new McpConfigurationManager(f.home, 'host-a', 'https://hive.example', 'secret', defaults.mcp);
  await manager.bootstrap();
  assert.equal(manager.status.state, 'applied');
  assert.match(await readFile(config, 'utf8'), /\[mcp_servers\."ivy_dev"\][^]*?enabled = true/);
  f.put('mcp', { ...defaults.mcp, hostId: 'host-a', servers: defaults.mcp.servers.map(server =>
    server.name === 'ivy_dev' ? { ...server, enabled: false } : server) });
  await manager.synchronize(f.connection);
  assert.equal(manager.status.state, 'applied');
  assert.match(await readFile(config, 'utf8'), /\[mcp_servers\."ivy_dev"\][^]*?enabled = false/);
});

test("AgentManager preserves Codex additions inside its TOML markers", async (t) => {
  for (const missingEnd of [false, true]) {
    const f = await fixture(t), config = join(f.home, 'config.toml');
    const defaults = await loadEnvironmentDefaults('', 'https://hive.example');
    const manager = new McpConfigurationManager(f.home, 'host-a', 'https://hive.example', 'secret', defaults.mcp);
    await manager.bootstrap();
    assert.equal(manager.status.state, 'applied');
    const original = await readFile(config, 'utf8');
    const altered = original
      .replace('default_permissions = ":danger-full-access"\n\n',
        'default_permissions = ":danger-full-access"\n\nnotify = ["computer-use", "turn-ended"]\nweb_search = "live"\n')
      .replace('# <<< Ivy AgentManager Codex configuration',
        '[mcp_servers.node_repl]\ncommand = "node_repl"\n' +
        (missingEnd ? '' : '# <<< Ivy AgentManager Codex configuration'));
    await writeFile(config, altered);
    await manager.synchronize(f.connection);
    assert.equal(manager.status.state, 'applied');
    const repaired = await readFile(config, 'utf8');
    assert.match(repaired, /notify = \["computer-use", "turn-ended"\]/);
    assert.match(repaired, /web_search = "live"/);
    assert.ok(repaired.indexOf('notify = ') < repaired.indexOf('# >>> Ivy AgentManager'));
    assert.ok(repaired.indexOf('[mcp_servers.node_repl]') > repaired.indexOf('# <<< Ivy AgentManager'));
    assert.equal((repaired.match(/# >>> Ivy AgentManager/g) ?? []).length, 1);
    assert.equal((repaired.match(/# <<< Ivy AgentManager/g) ?? []).length, 1);
    await manager.synchronize(f.connection);
    assert.equal(await readFile(config, 'utf8'), repaired);
  }
});

test("AgentManager refuses altered Ivy permissions during TOML recovery", async (t) => {
  const f = await fixture(t), config = join(f.home, 'config.toml');
  const defaults = await loadEnvironmentDefaults('', 'https://hive.example');
  const manager = new McpConfigurationManager(f.home, 'host-a', 'https://hive.example', 'secret', defaults.mcp);
  await manager.bootstrap();
  const altered = (await readFile(config, 'utf8'))
    .replace('approval_policy = "never"', 'approval_policy = "on-request"')
    .replace('# <<< Ivy AgentManager Codex configuration',
      '[mcp_servers.node_repl]\ncommand = "node_repl"\n# <<< Ivy AgentManager Codex configuration');
  await writeFile(config, altered);
  await manager.synchronize(f.connection);
  assert.equal(manager.status.state, 'conflict');
  assert.equal(await readFile(config, 'utf8'), altered);
});

test("AgentManager rejects unmanaged permission and hosted Ivy app conflicts", async (t) => {
  const defaults: Agent.McpDocument = {
    schemaVersion: 1,
    hostId: null,
    enabled: true,
    servers: [
      {
        name: "ivy",
        url: "https://hive.example/mcp",
        enabled: true,
        authentication: "agent-manager",
        startupTimeoutSeconds: 15,
        toolTimeoutSeconds: 45,
      },
    ],
  };
  for (const source of [
    'approval_policy = "on-request"\n',
    'sandbox_mode = "workspace-write"\n',
    "sandbox_workspace_write = { network_access = true }\n",
    "[sandbox_workspace_write]\nnetwork_access = true\n",
    '[apps."asdk_app_6aaab03ab3f48191a8e0e4dbdc434e5d"]\nenabled = true\n',
  ]) {
    const f = await fixture(t),
      config = join(f.home, "config.toml");
    await writeFile(config, source);
    const manager = new McpConfigurationManager(
      f.home,
      "host-a",
      "https://hive.example",
      "secret",
      defaults,
    );
    await manager.bootstrap();
    assert.equal(manager.status.state, "conflict");
    assert.equal(await readFile(config, "utf8"), source);
  }
});

test("AgentManager adopts matching unmanaged permissions and still disables the hosted Ivy app", async (t) => {
  const f = await fixture(t), config = join(f.home, "config.toml");
  await writeFile(config, 'approval_policy = "never"\ndefault_permissions = ":danger-full-access"\nmodel = "existing"\n');
  const manager = new McpConfigurationManager(f.home, "host-a", "https://hive.example", "secret", {
    schemaVersion: 1, hostId: null, enabled: true,
    servers: [{ name: "ivy", url: "https://hive.example/mcp", enabled: true,
      authentication: "agent-manager", startupTimeoutSeconds: 15, toolTimeoutSeconds: 45 }],
  });
  await manager.bootstrap();
  assert.equal(manager.status.state, "applied");
  const output = await readFile(config, "utf8");
  assert.equal((output.match(/^approval_policy\s*=/gm) ?? []).length, 1);
  assert.equal((output.match(/^default_permissions\s*=/gm) ?? []).length, 1);
  assert.match(output, /# >>> Ivy AgentManager Codex configuration/);
  assert.equal((output.match(/# >>> Ivy AgentManager/g) ?? []).length, 1);
  assert.ok(output.indexOf('model = "existing"') < output.indexOf('# >>> Ivy AgentManager'));
  assert.match(output, /model = "existing"/);
  assert.match(output, /\[apps\."asdk_app_6aaab03ab3f48191a8e0e4dbdc434e5d"\]\nenabled = false/);
  await manager.bootstrap();
  assert.equal(manager.status.state, "applied");
  assert.equal(await readFile(config, "utf8"), output);
});

test("AgentManager migrates both legacy TOML blocks and their state", async (t) => {
  const defaults: Agent.McpDocument = {
      schemaVersion: 1,
      hostId: null,
      enabled: true,
      servers: [
        {
          name: "ivy",
          url: "https://hive.example/mcp",
          enabled: true,
          authentication: "agent-manager",
          startupTimeoutSeconds: 15,
          toolTimeoutSeconds: 45,
        },
      ],
    };
  for (const hasPermissions of [true, false]) {
    const f = await fixture(t), config = join(f.home, "config.toml"),
      statePath = join(f.home, ".ivy-agent-mcp", "state.json");
    const manager = new McpConfigurationManager(
      f.home, "host-a", "https://hive.example", "secret", defaults,
    );
    await manager.bootstrap();
    const current = await readFile(config, "utf8");
    const appAndMcp = current.slice(current.indexOf('[apps."asdk_app_'), current.indexOf('# <<< Ivy AgentManager Codex configuration')).trimEnd();
    const oldMcp = ['# >>> Ivy AgentManager MCP configuration (managed; edit in AgentUI)',
      appAndMcp, '# <<< Ivy AgentManager MCP configuration'].join('\n');
    const oldPermissions = ['# >>> Ivy AgentManager Codex permissions (managed; edit in AgentUI)',
      'approval_policy = "never"', 'default_permissions = ":danger-full-access"',
      '# <<< Ivy AgentManager Codex permissions'].join('\n');
    const legacy = `${hasPermissions ? oldPermissions + '\n\n' : ''}model = "existing"\n\n${oldMcp}\n`;
    await writeFile(config, legacy);
    const state = JSON.parse(await readFile(statePath, "utf8"));
    state.schemaVersion = 1;
    state.appliedBlockHash = digest(oldMcp);
    if (hasPermissions) state.appliedPermissionsBlockHash = digest(oldPermissions);
    await writeFile(statePath, JSON.stringify(state));
    if (hasPermissions) {
      const edited = legacy.replace('default_permissions = ":danger-full-access"',
        'default_permissions = ":read-only"');
      await writeFile(config, edited);
      await manager.synchronize(f.connection);
      assert.equal(manager.status.state, "conflict");
      assert.equal(await readFile(config, "utf8"), edited);
      await writeFile(config, legacy);
    }
    await manager.synchronize(f.connection);
    assert.equal(manager.status.state, "applied");
    const output = await readFile(config, "utf8");
    assert.equal((output.match(/# >>> Ivy AgentManager/g) ?? []).length, 1);
    assert.match(output, /default_permissions = ":danger-full-access"/);
    assert.ok(output.indexOf('model = "existing"') < output.indexOf('# >>> Ivy AgentManager'));
    assert.equal(JSON.parse(await readFile(statePath, "utf8")).schemaVersion, 2);
    await manager.bootstrap();
    assert.equal(await readFile(config, "utf8"), output);
  }
});

test("AgentManager applies skill patches beside existing skills and releases omitted patches", async (t) => {
  const f = await fixture(t),
    other = join(f.skillsRoot, "personal");
  await mkdir(other);
  await writeFile(join(other, "SKILL.md"), "personal");
  const { skills } = await loadEnvironmentDefaults(
    f.root,
    "https://hive.example",
  );
  assert.ok(skills.files.every((file) => !file.path.startsWith("hive/")));
  const hive = {
    path: "hive/SKILL.md",
    content: "---\nname: hive\ndescription: Fixture.\n---\nUse Hive.\n",
  };
  const optional = {
    path: "optional/SKILL.md",
    content:
      "---\nname: optional\ndescription: Optional fixture.\n---\nUse the optional fixture.\n",
  };
  const previous: Agent.SkillsDocument = {
    ...skills,
    files: [
      ...skills.files,
      hive,
      optional,
      {
        path: "development/SKILL.md",
        content:
          "---\nname: development\ndescription: Development fixture.\n---\nDevelop Ivy.\n",
      },
      { path: "development/references/cli.md", content: "# CLI\n" },
    ],
  };
  const installed = new SkillsManager(
    f.home,
    f.skillsRoot,
    "host-a",
    "https://hive.example",
    previous,
  );
  await installed.bootstrap();
  assert.equal(installed.status.state, "applied");
  assert.match(
    await readFile(
      join(f.skillsRoot, "development", "references", "cli.md"),
      "utf8",
    ),
    /CLI/,
  );
  const development = join(f.skillsRoot, "development", "SKILL.md");
  await writeFile(development, "local development update");
  const manager = new SkillsManager(
    f.home,
    f.skillsRoot,
    "host-a",
    "https://hive.example",
    skills,
  );
  const replacement: Agent.SkillsDocument = {
    ...skills,
    files: [...skills.files, hive, optional],
  };
  f.put("skills", replacement);
  await manager.synchronize(f.connection);
  assert.equal(manager.status.state, "applied");
  for (const name of ["brave-container", "hive", "optional"])
    assert.equal(
      await readFile(
        join(f.skillsRoot, name, "SKILL.md"),
        "utf8",
      ),
      replacement.files.find((file) => file.path === name + "/SKILL.md")!
        .content,
    );
  assert.match(
    await readFile(
      join(
        f.skillsRoot,
        "brave-container",
        "scripts",
        "brave-container.ps1",
      ),
      "utf8",
    ),
    /Read-SessionTab/,
  );
  assert.equal(await readFile(development, "utf8"), "local development update");
  assert.equal(await readFile(join(other, "SKILL.md"), "utf8"), "personal");
  f.put("skills", {
    schemaVersion: 1,
    hostId: null,
    enabled: true,
    files: [
      {
        path: "hive/SKILL.md",
        content: "---\nname: hive\ndescription: Updated.\n---\nUpdated.\n",
      },
    ],
  });
  await manager.synchronize(f.connection);
  assert.match(
    await readFile(join(f.skillsRoot, "hive", "SKILL.md"), "utf8"),
    /Updated/,
  );
  const released = join(f.skillsRoot, "optional", "SKILL.md");
  assert.equal(await readFile(released, "utf8"), optional.content);
  await writeFile(released, "personal update");
  await manager.synchronize(f.connection);
  assert.equal(manager.status.state, "applied");
  assert.equal(await readFile(released, "utf8"), "personal update");
  assert.equal(await readFile(join(other, "SKILL.md"), "utf8"), "personal");
});

test("AgentManager migrates prefixed skills and state after a Codex home change", async (t) => {
  const f = await fixture(t);
  const previous: Agent.SkillsDocument = {
    schemaVersion: 1, hostId: null, enabled: true,
    files: [{ path: "hive/SKILL.md", content: "---\nname: hive\ndescription: Previous.\n---\nPrevious.\n" }],
  };
  const oldManager = new SkillsManager(f.home, f.skillsRoot, "host-a", "https://hive.example", previous);
  await oldManager.bootstrap();
  assert.equal(oldManager.status.state, "applied");
  const root = resolve(f.skillsRoot);
  const stableState = join(dirname(root), ".ivy-agent-skills-" + digest(root).slice(7, 23), "state.json");
  const currentState = JSON.parse(await readFile(stableState, "utf8")) as {
    authority: string; applied: Record<string, string>; appliedVersion: string;
  };
  await rename(join(root, "hive"), join(root, "ivy-managed-hive"));
  await writeFile(stableState, JSON.stringify({ ...currentState, schemaVersion: 1,
    applied: { "ivy-managed-hive": currentState.applied.hive } }));
  const legacyRoot = join(f.home, ".ivy-agent-skills");
  await mkdir(legacyRoot);
  await rename(stableState, join(legacyRoot, "state.json"));
  const migrated = new SkillsManager(f.home, f.skillsRoot, "host-a", "https://hive.example", previous);
  await migrated.synchronize(f.connection);
  assert.equal(migrated.status.state, "applied");
  assert.match(await readFile(join(root, "hive", "SKILL.md"), "utf8"), /Previous/);
  await assert.rejects(readFile(join(root, "ivy-managed-hive", "SKILL.md")), { code: "ENOENT" });
  const newState = JSON.parse(await readFile(stableState, "utf8"));
  assert.equal(newState.schemaVersion, 2);
  assert.deepEqual(Object.keys(newState.applied), ["hive"]);

  const nextHome = join(f.root, "next-codex-home");
  await mkdir(nextHome);
  const updated: Agent.SkillsDocument = {
    ...previous,
    files: [{ path: "hive/SKILL.md", content: "---\nname: hive\ndescription: Current.\n---\nCurrent.\n" }],
  };
  const relocated = new SkillsManager(nextHome, f.skillsRoot, "host-a", "https://hive.example", updated);
  await relocated.synchronize(f.connection);
  assert.equal(relocated.status.state, "applied");
  assert.match(await readFile(join(f.skillsRoot, "hive", "SKILL.md"), "utf8"), /Current/);
  assert.equal(JSON.parse(await readFile(stableState, "utf8")).schemaVersion, 2);
});

test("AgentManager leaves omitted prefixed skills alone and rejects an occupied plain name", async (t) => {
  const f = await fixture(t), root = resolve(f.skillsRoot);
  const previous: Agent.SkillsDocument = {
    schemaVersion: 1, hostId: null, enabled: true,
    files: [
      { path: "hive/SKILL.md", content: "legacy hive" },
      { path: "optional/SKILL.md", content: "legacy optional" },
    ],
  };
  const installed = new SkillsManager(f.home, root, "host-a", "https://hive.example", previous);
  await installed.bootstrap();
  const statePath = join(dirname(root), ".ivy-agent-skills-" + digest(root).slice(7, 23), "state.json");
  const state = JSON.parse(await readFile(statePath, "utf8")) as {
    authority: string; applied: Record<string, string>; appliedVersion: string;
  };
  for (const name of ["hive", "optional"]) await rename(join(root, name), join(root, "ivy-managed-" + name));
  await writeFile(statePath, JSON.stringify({ ...state, schemaVersion: 1,
    applied: { "ivy-managed-hive": state.applied.hive, "ivy-managed-optional": state.applied.optional } }));
  const omitted = join(root, "ivy-managed-optional", "SKILL.md");
  await writeFile(omitted, "local optional update");
  await mkdir(join(root, "hive"));
  await writeFile(join(root, "hive", "SKILL.md"), "personal hive");
  const current: Agent.SkillsDocument = { ...previous, files: previous.files.slice(0, 1) };
  const manager = new SkillsManager(f.home, root, "host-a", "https://hive.example", current);
  await manager.synchronize(f.connection);
  assert.equal(manager.status.state, "conflict");
  assert.equal(await readFile(join(root, "hive", "SKILL.md"), "utf8"), "personal hive");
  assert.equal(await readFile(join(root, "ivy-managed-hive", "SKILL.md"), "utf8"), "legacy hive");
  await rm(join(root, "hive"), { recursive: true });
  await manager.synchronize(f.connection);
  assert.equal(manager.status.state, "applied");
  assert.equal(await readFile(join(root, "hive", "SKILL.md"), "utf8"), "legacy hive");
  assert.equal(await readFile(omitted, "utf8"), "local optional update");
  const updated = JSON.parse(await readFile(statePath, "utf8"));
  assert.equal(updated.schemaVersion, 2);
  assert.deepEqual(Object.keys(updated.applied), ["hive"]);
});

test("AgentManager waits for a host override before migrating existing skills", async (t) => {
  const f = await fixture(t), root = resolve(f.skillsRoot);
  const hostSkills: Agent.SkillsDocument = {
    schemaVersion: 1, hostId: null, enabled: true,
    files: [
      { path: "hive/SKILL.md", content: "host hive" },
      { path: "secretary/SKILL.md", content: "host secretary" },
    ],
  };
  const installed = new SkillsManager(f.home, root, "host-a", "https://hive.example", hostSkills);
  await installed.bootstrap();
  const statePath = join(dirname(root), ".ivy-agent-skills-" + digest(root).slice(7, 23), "state.json");
  const state = JSON.parse(await readFile(statePath, "utf8")) as {
    authority: string; applied: Record<string, string>; appliedVersion: string;
  };
  for (const name of ["hive", "secretary"]) await rename(join(root, name), join(root, "ivy-managed-" + name));
  await writeFile(statePath, JSON.stringify({ ...state, schemaVersion: 1,
    applied: { "ivy-managed-hive": state.applied.hive, "ivy-managed-secretary": state.applied.secretary } }));
  const packaged: Agent.SkillsDocument = {
    ...hostSkills,
    files: [
      { path: "brave-container/SKILL.md", content: "packaged brave" },
      { path: "hive/SKILL.md", content: "packaged hive" },
    ],
  };
  const manager = new SkillsManager(f.home, root, "host-a", "https://hive.example", packaged);
  await manager.bootstrap();
  assert.equal(JSON.parse(await readFile(statePath, "utf8")).schemaVersion, 1);
  await assert.rejects(readFile(join(root, "brave-container", "SKILL.md")), { code: "ENOENT" });
  assert.equal(await readFile(join(root, "ivy-managed-secretary", "SKILL.md"), "utf8"), "host secretary");
  f.put("skills", { ...hostSkills, hostId: "host-a" });
  await manager.synchronize(f.connection);
  assert.equal(manager.status.state, "applied");
  assert.equal(await readFile(join(root, "secretary", "SKILL.md"), "utf8"), "host secretary");
  await assert.rejects(readFile(join(root, "brave-container", "SKILL.md")), { code: "ENOENT" });
  await assert.rejects(readFile(join(root, "ivy-managed-secretary", "SKILL.md")), { code: "ENOENT" });
  assert.deepEqual(Object.keys(JSON.parse(await readFile(statePath, "utf8")).applied), ["hive", "secretary"]);
});

test("AgentManager does not adopt an unowned skill directory", async (t) => {
  const f = await fixture(t), path = join(f.skillsRoot, "hive", "SKILL.md");
  await mkdir(dirname(path));
  await writeFile(path, "personal data");
  const manager = new SkillsManager(f.home, f.skillsRoot, "host-a", "https://hive.example", {
    schemaVersion: 1, hostId: null, enabled: true,
    files: [{ path: "hive/SKILL.md", content: "---\nname: hive\ndescription: Managed.\n---\nManaged.\n" }],
  });
  await manager.bootstrap();
  assert.equal(manager.status.state, "conflict");
  assert.equal(await readFile(path, "utf8"), "personal data");
});
