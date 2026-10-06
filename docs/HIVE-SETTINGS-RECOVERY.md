# Hive settings backup and restore

The Hive package includes a host-only offline command at
`dist/services/hive/src/settings-recovery.js`. It is deliberately not an MCP
operation. Stop the owning Hive process before using it. Keep every snapshot
under a private directory: it contains the **entire Hive database, including
secrets and non-settings data**. The command sets new snapshots to mode `0600`.

```sh
node dist/services/hive/src/settings-recovery.js backup STOPPED_HIVE.sqlite PRIVATE_BACKUP.sqlite
node dist/services/hive/src/settings-recovery.js plan PRIVATE_BACKUP.sqlite STOPPED_HIVE.sqlite sha256:BACKUP_HASH
node dist/services/hive/src/settings-recovery.js restore PRIVATE_BACKUP.sqlite STOPPED_HIVE.sqlite sha256:BACKUP_HASH PRIVATE_SAFETY.sqlite
```

`backup` verifies format 4 and SQLite integrity, then prints the backup hash
and a count by settings contract. Retain that hash independently of the file.
`plan` verifies both databases and the exact hash without writing anything.
`restore` refuses conflicts and creates a separate, verified full safety
snapshot of the target before changing it. It restores only missing Objects
and all their revisions for these contracts:

- `ivy/host-configuration`
- `agent/instructions`, `agent/mcp-configuration`, `agent/skills`
- `secretary/configuration`, `secretary/assignment`

An existing Object is preserved, even when its content differs from the
backup; `preserveDifferent` reports that case. Path, identity, immutable
contract, missing-parent, and revision-reference conflicts stop restoration.
Tickets, wiki pages, messages, and other Objects are never restored by this
command. It does not convert earlier storage formats or replace a full Hive
disaster-recovery restore. After restarting Hive, verify its health and the
affected service registrations and interactions before resuming work.
