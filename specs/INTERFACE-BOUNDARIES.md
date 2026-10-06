# Interface boundaries

The [operation catalog](schemas/operations.json) and [schemas](schemas) define the
protocol; [the SDK](../packages/sdk/src) is the public implementation boundary.

- HTTP and WebSocket use the same JSON-RPC operation frames. MCP adapts those operations
  and registered service definitions to its standard envelope; it adds no business API.
- Services use the SDK for registration, calls, events and notifications. Browser code
  uses the browser SDK. Consumers MUST NOT import Hive/contract internals or construct
  internal Hive requests when a canonical SDK operation exists.
- Registries own namespace versions, tool definitions, discovery metadata, inventory
  projections and notification schemas. Adding a service must not add service-specific
  fields or name switches to Hive.
- Stable consumers bind service name, namespace and interface version. Runtime calls
  retain the selected provider and exact definition hash. Native bindings additionally
  pin the installed catalog. A changed binding requires reassessment before dispatch.
  NativeOwner may refresh the stable Agent envelope once after confirmed non-execution,
  retaining the provider, interface version, native pins and original request, and rerunning
  domain guards. Uncertain outcomes and changed native definitions never authorize replay.
- Validate frames and routing at ingress, registries/configuration on admission, Objects
  on write and transient notification payloads before forwarding. Do not repeat domain
  payload validation at every transport hop or revalidate committed Objects on normal reads.
- Durable delivery uses the event journal's pull/replay/ACK path. WebSocket notifications
  are explicitly subscribed, transient hints that may prompt an authoritative read.
- Archive bytes, static assets, bootstrap package downloads, health and OAuth are explicit
  HTTP data planes. Interactive catalog reads and upload authorization remain operations.
- Native protocols and external integrations stay with their service owner. Cross-component
  results return through SDK tools, Objects, inventory or declared events/notifications.
  Offline storage inspection and recovery belong to the stopped owner's host lifecycle.

The executable import/network boundary and its concrete exceptions are maintained in
[SDK tests](../tests/sdk.test.ts), not in a second prose inventory.
