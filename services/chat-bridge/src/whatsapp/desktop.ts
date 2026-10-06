import { spawn } from "node:child_process";
import { WhatsAppJournal } from "./journal.js";
import { requireThat } from "../../../../packages/sdk/src/node.js";
import { translator } from "./messages.js";
import type { Translator } from "./messages.js";
/** Fixed program; no chat text is interpreted as PowerShell or a process selector. */
export async function desktopCommand(
  action: string,
  key: string,
  journal: WhatsAppJournal,
  t: Translator = translator(),
): Promise<string> {
  requireThat(
    action === "ensure" || action === "restart",
    "invalid_arguments",
    t("desktop.usage"),
  );
  requireThat(
    process.platform === "win32",
    "desktop_unavailable",
    t("desktop.platform"),
  );
  const stored = journal.get<{ attempted: boolean; result?: string }>(
    "desktop:" + key,
  );
  if (stored?.result) return stored.result;
  requireThat(
    !stored?.attempted,
    "desktop_outcome_unknown",
    t("desktop.unclear"),
  );
  const script = `$ErrorActionPreference='Stop'
$packages=@(Get-AppxPackage -Name 'OpenAI.Codex')
if($packages.Count -ne 1 -or $packages[0].Status -ne 'Ok'){throw 'Desktop package unavailable or servicing'}
$package=$packages[0]
$session=(Get-Process -Id $PID).SessionId
$root=[IO.Path]::GetFullPath($package.InstallLocation).TrimEnd('\\')+'\\'
$targets=@(Get-Process -Name Codex -ErrorAction SilentlyContinue | Where-Object {$_.SessionId -eq $session -and $_.Path -and $_.Path.StartsWith($root,[StringComparison]::OrdinalIgnoreCase)})
${
  action === "restart"
    ? `foreach($target in $targets){$target.CloseMainWindow() | Out-Null}
foreach($target in $targets){if(-not $target.WaitForExit(10000)){throw 'Desktop did not close gracefully'}}`
    : ""
}
if(${action === "restart" ? "$true" : "$targets.Count -eq 0"}){
 $manifest=Get-AppxPackageManifest -Package $package.PackageFullName
 $ui=@($manifest.Package.Applications.Application)[0].Id
 if(-not $ui){throw 'Desktop application missing'}
 Start-Process -FilePath explorer.exe -ArgumentList ('shell:AppsFolder\\'+$package.PackageFamilyName+'!'+$ui) -WindowStyle Hidden
}
Write-Output 'ok'`;
  journal.set("desktop:" + key, { attempted: true });
  await journal.flush();
  await new Promise<void>((resolve, reject) => {
    const child = spawn(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        Buffer.from(script, "utf16le").toString("base64"),
      ],
      { windowsHide: true, stdio: "ignore", shell: false },
    );
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error("Desktop operation timed out"));
    }, 30000);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      if (code === 0) resolve();
      else reject(new Error("Desktop operation failed"));
    });
  });
  const result = t(action === "restart" ? "desktop.restart" : "desktop.ensure");
  journal.set("desktop:" + key, { attempted: true, result });
  return result;
}
