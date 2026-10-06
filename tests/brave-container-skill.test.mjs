import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const isWindows = process.platform === "win32";
const powershell = isWindows
  ? join(
      process.env.SystemRoot ?? "C:\\Windows",
      "System32",
      "WindowsPowerShell",
      "v1.0",
      "powershell.exe",
    )
  : "powershell.exe";
const script = fileURLToPath(
  new URL(
    "../instructions/skills/brave-container/scripts/brave-container.ps1",
    import.meta.url,
  ),
);

function command(id, payload) {
  const header = Buffer.alloc(3);
  header.writeUInt16LE(payload.length + 1, 0);
  header.writeUInt8(id, 2);
  return Buffer.concat([header, payload]);
}

function tabWindow(tabId, windowId) {
  const payload = Buffer.alloc(8);
  payload.writeInt32LE(windowId, 0);
  payload.writeInt32LE(tabId, 4);
  return command(0, payload);
}

function tabNavigation(tabId, text) {
  const encoded = Buffer.from(text, "latin1");
  const payload = Buffer.alloc(8 + encoded.length);
  payload.writeUInt32LE(payload.length - 4, 0);
  payload.writeInt32LE(tabId, 4);
  encoded.copy(payload, 8);
  return command(6, payload);
}

function session(...commands) {
  const header = Buffer.alloc(8);
  header.write("SNSS", 0, "ascii");
  header.writeInt32LE(3, 4);
  return Buffer.concat([header, ...commands]);
}

function powerShellLiteral(value) {
  return `'${value.replaceAll("'", "''")}'`;
}

function run(root, ...args) {
  return spawnSync(
    powershell,
    [
      "-NoLogo",
      "-NoProfile",
      "-NonInteractive",
      "-File",
      script,
      ...args,
      "-UserDataDirectory",
      root,
      "-ProfileDirectory",
      "Default",
    ],
    { encoding: "utf8" },
  );
}

test(
  "bundled Brave container helper lists containers and identifies tab context",
  { skip: !isWindows },
  (t) => {
    const root = mkdtempSync(join(tmpdir(), "brave-container-skill-"));
    t.after(() => rmSync(root, { recursive: true, force: true }));

    const profile = join(root, "Default");
    const sessions = join(profile, "Sessions");
    mkdirSync(sessions, { recursive: true });
    writeFileSync(
      join(profile, "Preferences"),
      JSON.stringify({
        account_values: {
          brave: {
            containers: {
              list: [{ id: "container-work", name: "Work" }],
              used: [{ id: "container-archive", name: "Archive" }],
            },
          },
        },
      }),
    );

    writeFileSync(
      join(sessions, "Session_1"),
      session(
        tabWindow(101, 42),
        tabNavigation(
          101,
          "https://example.test/\0containers+container-work:https://example.test/",
        ),
        tabWindow(102, 42),
        tabNavigation(102, "https://example.test/default"),
      ),
    );

    const listed = run(root, "list");
    assert.equal(listed.status, 0, listed.stderr);
    assert.deepEqual(JSON.parse(listed.stdout), {
      profileDirectory: "Default",
      containers: [
        { id: "container-archive", name: "Archive", source: "used" },
        { id: "container-work", name: "Work", source: "configured" },
      ],
    });

    const contained = run(root, "identify", "-TabId", "101", "-Attempts", "1");
    assert.equal(contained.status, 0, contained.stderr);
    assert.deepEqual(JSON.parse(contained.stdout), {
      tabId: 101,
      windowId: 42,
      profileDirectory: "Default",
      container: {
        id: "container-work",
        name: "Work",
        source: "configured",
      },
      context: "container",
      evidence: "brave-session",
    });

    const defaultContext = run(
      root,
      "identify",
      "-TabId",
      "102",
      "-Attempts",
      "1",
    );
    assert.equal(defaultContext.status, 0, defaultContext.stderr);
    assert.equal(JSON.parse(defaultContext.stdout).context, "default");

    const missing = run(root, "identify", "-TabId", "999", "-Attempts", "1");
    assert.notEqual(missing.status, 0);
    assert.equal(JSON.parse(missing.stderr).code, "container_tab_not_found");
  },
);

test(
  "Brave open reports only a uniquely observed new native tab ID",
  { skip: !isWindows },
  (t) => {
    const root = mkdtempSync(join(tmpdir(), "brave-container-open-"));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    const profile = join(root, "Default");
    const sessions = join(profile, "Sessions");
    mkdirSync(sessions, { recursive: true });
    writeFileSync(
      join(profile, "Preferences"),
      JSON.stringify({
        brave: {
          containers: { list: [{ id: "container-work", name: "Work" }] },
        },
      }),
    );
    writeFileSync(
      join(sessions, "Session_1"),
      session(
        tabWindow(101, 42),
        tabNavigation(
          101,
          "containers+container-work:https://example.test/old",
        ),
      ),
    );

    function openWithMock(nextSession, attempts = 2) {
      const destination = join(sessions, "Session_2");
      const source = join(root, "NextSession");
      if (nextSession) writeFileSync(source, nextSession);
      const copy = nextSession
        ? `Copy-Item -LiteralPath ${powerShellLiteral(source)} -Destination ${powerShellLiteral(destination)}`
        : "";
      const powerShellCommand = `
        function Start-Process {
          param([string] $FilePath, [string[]] $ArgumentList, [switch] $PassThru)
          ${copy}
          $process = [pscustomobject]@{ ExitCode = 0 }
          $process | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value { param($milliseconds) return $true }
          return $process
        }
        . ${powerShellLiteral(script)} open -Container Work -Url https://example.test/new -BraveExecutable "$PSHOME\\powershell.exe" -UserDataDirectory ${powerShellLiteral(root)} -ProfileDirectory Default -ObservationAttempts ${attempts}
      `;
      return spawnSync(
        powershell,
        [
          "-NoLogo",
          "-NoProfile",
          "-NonInteractive",
          "-EncodedCommand",
          Buffer.from(powerShellCommand, "utf16le").toString("base64"),
        ],
        { encoding: "utf8" },
      );
    }

    const observed = openWithMock(
      session(
        tabWindow(103, 42),
        tabNavigation(
          103,
          "containers+container-work:https://example.test/new",
        ),
      ),
    );
    assert.equal(observed.status, 0, observed.stderr);
    assert.deepEqual(JSON.parse(observed.stdout), {
      launched: true,
      container: "Work",
      containerId: "container-work",
      url: "https://example.test/new",
      profileDirectory: "Default",
      tabId: 103,
      windowId: 42,
      tabObservation: "observed",
    });

    const ambiguous = openWithMock(
      session(
        tabWindow(104, 42),
        tabNavigation(
          104,
          "containers+container-work:https://example.test/new",
        ),
        tabWindow(105, 42),
        tabNavigation(
          105,
          "containers+container-work:https://example.test/new",
        ),
      ),
      2,
    );
    assert.equal(ambiguous.status, 0, ambiguous.stderr);
    assert.equal(JSON.parse(ambiguous.stdout).tabObservation, "ambiguous");
    assert.equal(JSON.parse(ambiguous.stdout).tabId, null);

    const wrongUrl = openWithMock(
      session(
        tabWindow(106, 42),
        tabNavigation(
          106,
          "containers+container-work:https://example.test/newer",
        ),
        tabWindow(107, 42),
        tabNavigation(
          107,
          "containers+container-other:https://example.test/new",
        ),
      ),
      2,
    );
    assert.equal(wrongUrl.status, 0, wrongUrl.stderr);
    assert.equal(JSON.parse(wrongUrl.stdout).tabObservation, "unconfirmed");
    assert.equal(JSON.parse(wrongUrl.stdout).tabId, null);
  },
);
