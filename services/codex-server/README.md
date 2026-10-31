# Shared Codex server

This optional native component runs one foreground shared Codex server under the
existing HostExecutor process owner. Configure it as its own process instance,
separate from AgentManager. Its native executable has the Windows GUI subsystem;
the host's existing Job launcher starts descendants without a console. Each actual
Codex home has one exclusive owner lock. AgentManager uses `external-proxy` with
the matching home and socket; restarting AgentManager closes only that proxy.

Settings are `nativeExecutable`, `nativeExecutableHash`, `nativeVersion`,
`codexHome`, `args`, `socketPath`, `windowsShell` and optional `legacyStartTasks`.
Paths and identities are installation inputs. `args` selects the pinned CLI's
foreground server command, `["app-server", "--listen", "unix://<socketPath>"]`.
Do not select a detached daemon-start command: this component must own the server.
Use the same verified unpackaged shell configuration as AgentManager. The native
CLI version/hash, proxy handshake, actual server version and home are verified
before readiness. Logs use the host runtime's bounded service log; health and
shutdown use its normal identity-bound files. It does not register a Hive service.

Before launch, Windows startup inspection rejects enabled definitions explicitly
listed in `legacyStartTasks` (full Scheduler paths), or definitions whose action
or bounded local PowerShell script identifies this home and a server start.
Other homes remain independent. Dynamic scripts that do not disclose their home
must be listed explicitly. A conflicting startup fails readiness and therefore
cannot produce a successful deployment.

For migration, first inspect the current definitions, their exact actions,
principals, running process identities, home and socket. Export the selected XML.
Prepare this component disabled through the normal host deployment path. At a
coordinated idle window disable only those identified legacy starts, then wait for
their verified owner shutdown before enabling the new instance. Do not stop
unrelated work or close existing windows as a migration shortcut. Verify the
original deployment, server identity, agent continuation and functional tools.
Keep the disabled old definitions until acceptance. To roll back, disable and
verify retirement of this instance before restoring the retained legacy start;
never run both owners for one home.

AgentManager can also opt into verified direct bundled tool starts with its
`bundledTools` setting. It contains an absolute `nodeExecutable`, `nodeEnvironment`
and `cuaEnvironment` string maps, `inheritedEnvironment`, `nodeArgs`, `cuaArgs`,
`startupTimeoutSeconds` and `toolTimeoutSeconds`. Copy the approved local MCP
environment, arguments and timeouts when migrating. The registered app and matching
cache are verified before each new thread start/resume/fork, and command, args and
environment are published together under the existing MCP output lock. Verified
runtime paths replace old version addresses; operator settings such as
`CODEX_CLI_PATH` remain explicit and unchanged.
Already loaded contexts keep their connections. Missing or mismatched runtimes
fail before native dispatch; open the registered app to provision its cache.

Back up `config.toml` and explicitly remove the old `node_repl`/`cua_repl` sections
before enabling this setting. The normal path refuses to adopt unmanaged sections
or overwrite changes in its own marked block. Preserve browser backend, trusted
service and environment allowlist settings; the former Node pipe relay is no
longer used. For rollback disable `bundledTools`, retain current contexts until
they finish, and restore the saved sections under the same MCP output lock.
