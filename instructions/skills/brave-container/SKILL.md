---
name: brave-container
description: List Brave containers, identify a Brave tab's container, or open an HTTP(S) URL in a named Brave container on Windows. Use the Brave Browser extension for tab selection, not page interaction.
---

# Brave containers

When connected, use the Brave Browser extension (`@Brave Browser`) for tab inventory and page
interaction. `@Browser` has a separate profile; `@Chrome` does not see Brave tabs. Use the bundled
`scripts/brave-container.ps1` for local container metadata and launching into a named container.
Do not read Brave's `Preferences` or `Sessions` files manually.

Resolve the script relative to this skill directory and run it with Windows PowerShell. All
successful commands emit one JSON object on stdout; failures emit a JSON error on stderr and exit
nonzero. Use `-UserDataDirectory`, `-ProfileDirectory`, or `-BraveExecutable` only when the default
Brave profile selection is not the intended one.

## List containers

```powershell
powershell.exe -NoLogo -NoProfile -NonInteractive -File "<skill-directory>\scripts\brave-container.ps1" list
```

Use only an exact, unique returned name. `source: "used"` means the container is still referenced
by the local profile but is not in its configured list.

## Identify a tab's container

Refresh the Brave extension's tab inventory and select the target unambiguously by URL and title.
Pass its numeric `id` as `-TabId`, never the composite `providerTabId` or a CDP target ID. Match
the script's profile to the extension's Brave profile.

```powershell
powershell.exe -NoLogo -NoProfile -NonInteractive -File "<skill-directory>\scripts\brave-container.ps1" identify -TabId 123
```

`context: "container"` includes the container ID and name. `context: "default"` is Brave's default
profile context. URL, login state, tab group, and visible content are not container evidence. If
the script reports `container_tab_not_found`, refresh the browser inventory once and retry with the
current native tab ID.

## Open a URL in a container

Opening a URL is a browser mutation and requires a user request that covers it.

```powershell
powershell.exe -NoLogo -NoProfile -NonInteractive -File "<skill-directory>\scripts\brave-container.ps1" open -Container "Exact name" -Url "https://example.com/"
```

Only HTTP(S) URLs are accepted. If connected, snapshot the Brave extension's tab inventory before
opening; then select the sole new matching tab, bind it by its numeric `id`, and verify its
container with `identify`. The script's `tabId` is best-effort and may be null even when the tab
opened; confirm any returned ID against the fresh inventory. Without the extension, opening still
works but plugin tab binding is unavailable. If selection is ambiguous or opening times out, inspect
available tabs before retrying; never infer an ID from the URL alone or close possible duplicates
without a user request.
