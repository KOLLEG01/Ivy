# Native AgentManager

Build with `cargo build --package ivy-agent-manager --bin ivy-agent-manager`.
Run `ivy-agent-manager --config <absolute-instance.json>`, using the existing
instance format, prepared build identity, native catalog and provider settings.
The backend starts without Node. Configured JavaScript provider workloads still
use their own explicit runtime.

`settings.backends` contains one to eight `{dataRoot, settings}` entries (see
`ServiceSettings` in the [agent schema](../../../specs/schemas/agent.schema.json)).
Each entry has its own native settings, explicit backend identity/connection mode and separate
data/native home. One host owns the Hive registration, health/control files and
shutdown. Native starts and Hive reconciliation run independently per backend;
admission requires that backend's current reconciled Hive generation. A failed
native connection retains its original operation queries while other backends
remain available. Hive must positively advertise native JSON parameters and its
pre-ready admission outcome through `system.status.nativeToolTransport`.

Reset, SQLite and catalog preparation also fail per backend. `agent.backends`
reports `phase: preparation` when no usable native owner was established;
unverified catalog hashes and journal counts are omitted. Such an entry exposes
no native tools. Original receipts remain queryable if SQLite and its account
binding were admitted (`operationReadsAvailable`); otherwise queries return the
preparation error without proving absence. Invalid cross-account settings reject the whole host
before reset or provider state is opened. With no available backend the host
remains unready; Hive receives each preparation diagnostic, while tool calls
retain the existing pre-ready rejection.

`settings.backend` identifies a backend by `backendId`, `provider` (`codex` or
`claude`) and a stable logical `accountId`. It is pinned in local state before
work is admitted. Its management calls select that complete identity in the
outer `backend` field; native parameters remain unchanged. `agent.backends`
lists the configured identities and their status. Native tool namespaces use
the backend ID, and SDK catalog caches and evidence checks retain the account.
An unscoped single owner uses its existing `codex` namespace and cannot select
another backend. Removing or reassigning a pinned account requires an explicit
state transfer that preserves its receipts.

The `agent.notification` and `agent.capabilities` topic payloads use version
`1.1.0` for the optional backend identity. Published `1.0.0` definitions remain
immutable in Hive.

Before combining existing instances, open each journal under its original unscoped
configuration to seed its stable storage owner and configured native provider.
Retained state cannot acquire an explicit account or move to another service
before that provider binding; subsequent provider changes are rejected. The
provider comes from the configured engine, never the protocol catalog's label.
Moving the RPC route then preserves
SQLite pins and original receipt bodies. The SDK's `original_operations` /
`originalOperations` handle reads those receipts through a selected current
backend without changing their owner or granting dispatch authority. Current
route absence does not prove absence at a historical owner.

Inventory starts and recovers with a complete observation, then applies event deltas.
Periodic current-partition reads refresh only an unchanged set of ordinary current IDs;
different membership requires complete reconciliation rather than inferred deletion or
archive state. Historical and ephemeral entries retain their original observation times.
Complete consistency checks keep their own start time, independent of deltas and delayed
acknowledgements. The bounded work and intervals live in
[InventoryManager](src/inventory_manager.rs); history pages are read on demand.
Each native enumeration accepts at most 64 pages of 100 rows, with nonempty opaque
continuations limited to 8,192 UTF-16 units each and 512 KiB of retained cursor
strings and headers. Empty pages consume the same page budget. The existing
5,000-thread/loaded-ID and 256-project limits remain; an incomplete or invalid
scan retains the previous observation rather than publishing a partial snapshot.
Summaries accumulate directly into their final collection.

Run the same executable with `--health --config <absolute-instance.json>` for
the existing JSON readiness result. The probe validates fresh local health and
the matching connected, synchronized Hive registration without starting an agent.

The Rust process owns service registration, management/native handlers, SQLite,
health and control files. A matching control file or an OS termination signal
closes the owned process and drains service work before exit. Windows and Linux
use their native process adapters.
