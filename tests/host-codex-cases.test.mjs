import test from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
const exec = promisify(execFile);
const shell = join(
  process.env.SystemRoot ?? "C:/Windows",
  "System32/WindowsPowerShell/v1.0/powershell.exe",
);
const ps = async (text) => {
  try {
    return await exec(
      shell,
      [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        Buffer.from(text, "utf16le").toString("base64"),
      ],
      { windowsHide: true, timeout: 30000, maxBuffer: 65536 },
    );
  } catch (error) {
    throw Error(
      String(error.stderr || error.stdout || error.code).slice(-6000),
    );
  }
};
const literal = (value) => "'" + value.replaceAll("'", "''") + "'";
const quote = (value) =>
  '"' + value.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\+)$/, "$1$1") + '"';

test(
  "Windows startup audit detects selected legacy definitions and preserves other server homes",
  { skip: process.platform !== "win32" },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "ivy-start-audit-"));
    t.after(() => rm(root, { recursive: true, force: true }));
    const script = join(root, "start.ps1"),
      home = join(root, "selected");
    await writeFile(
      script,
      "$env:CODEX_HOME = '" + home + "'\n# codex ui-server\n",
    );
    const source = await readFile(
      "services/codex-server/rust/assets/inspect-starts.ps1",
      "utf8",
    );
    const code =
      `$ErrorActionPreference='Stop'\n${source}\nfunction Get-IvySharedServerTasks { $script:tasks }\n` +
      `$script:tasks=@([pscustomobject]@{TaskPath='\\';TaskName='old';State='Ready';Actions=@([pscustomobject]@{Execute='pwsh.exe';Arguments='-File "${script}"';WorkingDirectory=''})})\n` +
      `$inputJson=${literal(JSON.stringify({ home, legacyTasks: [] }))}\n` +
      `try {Assert-IvySharedServerStarts $inputJson;throw 'missed selected script'} catch {if($_.Exception.Message -notlike 'Enabled startup definition*'){throw}}\n` +
      `$verbatim=${literal(JSON.stringify({ home: "\\\\?\\" + home, legacyTasks: [] }))}\n` +
      `try {Assert-IvySharedServerStarts $verbatim;throw 'missed canonical home'} catch {if($_.Exception.Message -notlike 'Enabled startup definition*'){throw}}\n` +
      `$script:tasks[0].State='Disabled';Assert-IvySharedServerStarts $inputJson\n` +
      `$script:tasks[0].State='Ready';$script:tasks[0].Actions[0].Arguments='codex ui-server other-home';Assert-IvySharedServerStarts $inputJson\n` +
      `$inputJson=${literal(JSON.stringify({ home, legacyTasks: ["\\old"] }))}\n` +
      `try {Assert-IvySharedServerStarts $inputJson;throw 'missed named definition'} catch {if($_.Exception.Message -notlike 'Enabled startup definition*'){throw}}\nexit 0`;
    await ps(code);
  },
);

test(
  "Scheduler enters through the GUI Job launcher without a console and preserves an independent server owner",
  { skip: process.platform !== "win32", timeout: 90000 },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "ivy-headless-start-"));
    const launcher = resolve(
      process.env.CARGO_TARGET_DIR ?? "target",
      "debug/ivy-host-job.exe",
    );
    const image = await readFile(launcher);
    assert.equal(
      image.readUInt16LE(image.readUInt32LE(0x3c) + 24 + 68),
      2,
      "The actual Scheduler entry has the GUI subsystem",
    );
    const executable = join(root, "probe.exe"),
      stop = join(root, "stop");
    const compiler = join(
      process.env.SystemRoot ?? "C:/Windows",
      "Microsoft.NET/Framework64/v4.0.30319/csc.exe",
    );
    await exec(
      compiler,
      [
        "/nologo",
        "/target:exe",
        "/out:" + executable,
        resolve("tests/acceptance/fixtures/shared-server-probe.cs"),
      ],
      { windowsHide: true, timeout: 30000 },
    );
    const prefix = "Ivy-Test-" + randomUUID(),
      names = [prefix + "-server", prefix + "-connection"];
    const records = names.map((_, i) => join(root, i + ".json"));
    const scheduler =
      "$s=New-Object -ComObject 'Schedule.Service';$s.Connect();$folder=$s.GetFolder('\\');";
    const checked = (name) =>
      `$task=$folder.GetTask(${literal(name)});if($task.Definition.Actions.Count -ne 1 -or $task.Definition.Actions.Item(1).Path -ne ${literal(launcher)} -or $task.Definition.Actions.Item(1).Arguments -notlike ${literal("*" + root + "*")}){throw 'Test task ownership changed'};`;
    t.after(async () => {
      await ps(
        scheduler +
          names
            .map(
              (name) =>
                `try {${checked(name)}$task.Stop(0);$folder.DeleteTask(${literal(name)},0)} catch {if($_.Exception.HResult -ne -2147024894){throw}}`,
            )
            .join("\n"),
      );
      await new Promise((r) => setTimeout(r, 300));
      await rm(root, {
        recursive: true,
        force: true,
        maxRetries: 20,
        retryDelay: 100,
      });
    });
    await ps(
      scheduler +
        names
          .map((name, i) => {
            const args = [
              "--parent",
              "0",
              launcher,
              "--parent",
              "0",
              executable,
              records[i],
              stop,
            ]
              .map(quote)
              .join(" ");
            return `$d=$s.NewTask(0);$d.Principal.UserId=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;$d.Principal.LogonType=3;$d.Settings.Enabled=$true;$d.Settings.ExecutionTimeLimit='PT1M';$d.Settings.DisallowStartIfOnBatteries=$false;$d.Settings.StopIfGoingOnBatteries=$false;$a=$d.Actions.Create(0);$a.Path=${literal(launcher)};$a.Arguments=${literal(args)};$a.WorkingDirectory=${literal(root)};$task=$folder.RegisterTaskDefinition(${literal(name)},$d,2,$d.Principal.UserId,$null,3,$null);$null=$task.Run($null);`;
          })
          .join("\n"),
    );
    const observe = async (file, predicate = () => true) => {
      const end = Date.now() + 15000;
      while (Date.now() < end) {
        try {
          const v = JSON.parse(await readFile(file, "utf8"));
          if (predicate(v)) return v;
        } catch {}
        await new Promise((r) => setTimeout(r, 100));
      }
      throw Error("No matching isolated process observation");
    };
    const server = await observe(records[0]),
      connection = await observe(records[1]);
    assert.equal(server.console, 0);
    assert.equal(connection.console, 0);
    await ps(scheduler + checked(names[1]) + "$task.Stop(0);");
    const continued = await observe(
      records[0],
      (v) => v.tick > server.tick + 3,
    );
    assert.equal(
      continued.pid,
      server.pid,
      "Retiring the connection owner preserves the same server process and its work",
    );
    await writeFile(stop, "stop");
  },
);
