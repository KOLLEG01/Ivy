# Shared packages

| Package | Responsibility |
| --- | --- |
| [contracts](contracts) | Generated/shared protocol model |
| [sdk](sdk) | Public consumer API |
| [ui](ui) | Shared visual components |
| [ui-client](ui-client) | Ivy-specific browser helpers |
| [host-runtime](host-runtime) | Host lifecycle implementation |
| [cli](cli) | Independent local command-line client |

Service-specific code stays with its service. The SDK package is built from `packages/sdk`;
[publish-sdk-ref](../tools/package/publish-sdk-ref.mjs) publishes an immutable package Git ref.
External consumers pin the emitted package commit instead of copying SDK source or archives.
