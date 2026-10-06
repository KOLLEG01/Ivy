# Repository work

- Keep documentation compact: explain the project in README/docs and requirements in
  specs. Give each rule one home; link to code, schemas and tests for implementation
  details. Do not add reports, progress notes, handoffs or dated acceptance claims.
- Keep deployment identities, hostnames, account data, credentials and installation
  paths out of tracked documentation. Do not describe named external private
  repositories or their implementations here. Use neutral placeholders in examples.
- Deployment-specific instructions belong in the untracked `AGENTS.override.md`.
  Change that file only when explicitly requested.

## Language

- Use English as the default language throughout the repository and product,
  including documentation, code-facing text and user-facing copy.
- Keep the product capable of operating in other languages. Do not hard-code
  English assumptions that prevent localization or language-specific deployments.

## Architecture and implementation

- Discover implementations supplied by private repositories, including services,
  only at runtime through their registrations and advertised interfaces. Do not
  name, import, bundle, enumerate or special-case them in this repository;
  keep source, schemas, tests, docs and deployment plans implementation-agnostic.
- Preserve the application's established structure, separation of concerns and
  architecture. Deviations require the user's prior request or explicit approval.
- Apply KISS to application changes. Keep runtime behavior and the deployment,
  build and test paths lean and fast; justify any additional safeguard that may
  slow them down.
- Tell the user about relevant opportunities to simplify or improve the application.
  Present necessary tradeoffs against these rules to the user and obtain approval
  before implementing them.

## UI and UX

- Use the shared shadcn component system and keep the common theme aligned with the
  shadcn website. Do not create app-local substitutes for shared UI primitives.
- Apply UI/UX decisions that are not inherently specific to one app, including
  component-library, navigation and shared interaction changes, consistently to
  every applicable UI. Implement them as reusable components in the shared UI
  packages rather than duplicating them in individual apps.
- Treat the shared UI library like a document style system: app code composes shared
  shadcn primitives and patterns, while recurring typography, spacing, grouping,
  status, form and navigation treatments belong in the library. Prefer adding or
  adopting a shadcn component centrally over app-local markup and styling.
- Design and verify every UI and interaction for both desktop and mobile use.

## Changes and verification

- Before creating a TaskBoard ticket, read `task_configuration` and use its current
  defaults unless the user explicitly requests different settings.
- Test changed behavior and affected dependencies with `npm test -- --files PATH` or
  `--scope NAME`; inspect `--list` before a large selection. Selection, reuse and limits
  are defined in the [runner](tools/testing/test.mjs) and [configuration](tools/testing/test-config.mjs).
- Reuse existing cases; add tests only for a distinct failure risk. Build once per run
  and repeat passing checks only after relevant changes or new evidence.
- Run full regression only on explicit request or after approval that implementation
  is complete. Deployment and general testing remain separate.
- Keep successful checks quiet. Remove temporary diagnostics when resolved; do not
  retain test reports, screenshots or permanent test directories.
- Follow [local host operations](specs/DEPLOYMENT.md#local-host-operations) for host
  mutations and recovery. Diagnose the exact original operation and evidence path;
  do not scan all retained artifact history.

## Placement

Keep tools under `tools/{build,contracts,operations,package,testing}`, automated tests
under `tests` (browser tests in `tests/web-e2e`, installed/manual acceptance in
`tests/acceptance`). Component plans live at
`services/<component>/deploy.json` or `ui/<app>/deploy.json`. Service-specific native
and extension code stays with its service; reusable code belongs in `packages`.
Runtime instructions belong in `instructions`, repository skills in `.agents/skills`,
and source-only examples in `docs/examples`.
