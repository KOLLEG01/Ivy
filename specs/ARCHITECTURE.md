# Architecture requirements

[README](../README.md) introduces the components. MUST and MUST NOT denote requirements;
SHOULD denotes a default that needs a reason to deviate.

- Components MUST be independently buildable, replaceable and recoverable. Local host
  maintenance MUST remain available when Hive or an agent service is unavailable.
- Hive owns generic persistence, discovery and routing. Domain services own workflows,
  native resources and external effects. Hive MUST NOT execute uploaded service code,
  validators, workflows or build commands.
- One Hive storage owner opens its local SQLite database. Each component has one process
  owner; replacement must not create a second writer or duplicate external work.
- Public builds MUST work without external source checkouts, personal runtime data or
  credentials. Optional integrations use ordinary SDK contracts without Hive special cases.
- Host, service instance, principal and native resource identities are explicit and
  stable. Display names and process lifetimes are not identity. A native resource stays
  with its recorded owner; an outage never selects a substitute host.
- A Hive is one trusted domain. Authentication is not per-user role/ACL isolation;
  installed UIs share its authenticated origin. Separate trust domains require separate origins.
- Source, immutable releases, configuration and runtime data remain separate.
  Installation topology and personal data do not belong in source documentation or packages.
- The current product has no implicit data migration, legacy cutover or compatibility
  adapter. Unsupported retained data must block activation rather than be reset or rewritten.
- Persist product state and the minimum operation state needed for recovery. Diagnostics
  and process existence cannot establish successful execution or readiness.
- Long work returns an operation identity. Reconnect reconciles original work; it never
  proves that a possibly completed external effect is safe to repeat.

Boundary-specific requirements live in [interfaces](INTERFACE-BOUNDARIES.md),
[Hive](IVYHIVE-SPEC.md), [deployment](DEPLOYMENT.md) and [services](SERVICES.md).
