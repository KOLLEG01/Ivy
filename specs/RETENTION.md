# Retention requirements

Contract policy: [wire schema](schemas/hive-wire.schema.json).
Implementation: [collector](../services/hive/src/retention.ts),
[Object references](../services/hive/src/objects.ts), [mutation journal](../services/hive/src/store.ts).
Checks: [retention](../tests/retention.test.ts).

- Each contract declares Object and revision retention. The latest registered family policy
  governs collection; a new policy is previewed before a later pass applies it.
- Navigation parent and immutable ownership are distinct. Owning an Object does not imply
  that every nested JSON string is a reference; owners declare exact revision references.
- Keep the current revision and every explicitly referenced revision.
  Collect old eligible revisions without rewriting the remaining content.
- Delete an eligible Object only when its children, owned data and referencing Objects can
  be collected together. Retained references block deletion. Collection must
  preserve referential integrity and tolerate eligible reference cycles.
- Archival is a navigation state, not a deletion request. Status exposes eligible/protected
  counts, policy identity and collection failures; quota pressure must not erase pending work.
- Retain only current and previous app releases. Their static files live in private,
  versioned directories under the Hive data root and are served through the existing
  authenticated UI routes. Collection removes directories for older releases
  once those directories are a day old; the collector runs every 15 minutes.
  Immutable URLs are stable while retained, not a permanent availability promise.
- Keep at most the latest two uploaded `.tar.gz` app packages per app in the package
  catalog and artifact directory. Failed app uploads leave a local marker so orphaned
  archives are removed on startup or the next collection pass. Stale incoming package
  files and private UI staging directories are removed after a day. Service package
  retention is independent.
- The global event journal has finite age/count/byte bounds. Pruning publishes an explicit
  gap boundary even for lagging subscribers; consumers recover from authoritative snapshots.
- Mutation receipts have a finite replay window. Runtime epoch and issue time fence expired
  identities; expired or foreign identities fail instead of creating fresh work. A retained
  identical receipt resolves before rechecking newer domain state. Retain at most 100,000
  receipts and 256 MiB of encoded receipt data. The count provides more than 100 times the
  measured reference-day volume of 930 receipts; the byte bound permits about 2.6 KiB per
  receipt at that count, compared with the measured mean below 0.4 KiB. Admission reserves
  the full 64 KiB single-receipt maximum before applying an effect.
- Resolved diagnostics are retained for at most seven days and 10,000 identities, oldest
  observation first. Current, stale and unknown conditions are never removed for history
  pressure; their combined global admission bound is 4,096 identities. Repeated observations
  update the retained timestamp so age remains correct across restarts.
- One collection pass removes at most 500 eligible revisions per contract family. Remaining
  work advances on later passes; candidate selection is not repeated for every deletion batch.
- Use [SDK operation identities](../packages/contracts/src/operation-id.ts) and owner status
  for recovery. Receipt expiry, missing records and retention gaps never prove that an external
  effect did not happen. Domain owners retain unresolved work and its necessary evidence.
- Collection bounds and periods live in code. Runtime reset is separate from collection and
  follows [deployment recovery](DEPLOYMENT.md).
