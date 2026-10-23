# Native AgentManager

Build with `cargo build --package ivy-agent-manager --bin ivy-agent-manager`.
Run `ivy-agent-manager --config <absolute-instance.json>`, using the existing
instance format, prepared build identity, native catalog and provider settings.
The backend starts without Node. Configured JavaScript provider workloads still
use their own explicit runtime.

Run the same executable with `--health --config <absolute-instance.json>` for
the existing JSON readiness result. The probe validates fresh local health and
the matching connected, synchronized Hive registration without starting an agent.

The Rust process owns service registration, management/native handlers, SQLite,
health and control files. A matching control file or an OS termination signal
closes the owned process and drains service work before exit. Windows and Linux
use their native process adapters.
