# Retention requirements

Contract policy: [wire schema](schemas/hive-wire.schema.json).
Implementation: [collector](../services/hive/rust/src/retention.rs),
[Object references](../services/hive/rust/src/objects.rs), [mutation journal](../services/hive/rust/src/store.rs).
Checks: [retention](../tests/retention.test.ts).

- Each contract declares Object and revision retention. The latest registered family policy
  governs collection; a new policy is previewed before a later pass applies it.
- Service-owned policies may require owner readiness. Collection waits for a ready writer
  of that exact contract version after its storage migration; the durable acknowledgement
  is bound to the version and policy. Older owners cannot release a newer migration guard.
- Navigation parent and immutable ownership are distinct. Owning an Object does not imply
  that every nested JSON string is a reference; owners declare exact revision references.
- Keep the current revision and every explicitly referenced revision.
  Collect old eligible revisions without rewriting the remaining content.
- Delete an eligible Object only when its children, owned data and referencing Objects can
  be collected together. Retained references block deletion. Collection must
  preserve referential integrity and tolerate eligible reference cycles.
- Archival is a navigation state, not a deletion request. Status exposes eligible/protected
  counts, policy identity and collection failures; quota pressure must not erase pending work.
- Retain only current app release metadata. Their static files live in private,
  versioned directories under the Hive data root and are served through the existing
  authenticated UI routes. Collection removes directories for older releases
  once those directories are a day old; the collector runs every 15 minutes.
  Immutable URLs are stable while retained, not a permanent availability promise.
- Keep the latest uploaded `.tar.gz` package per component, plus builds referenced
  by Hive, registered services, host installation observations or unfinished deployments.
  Last observations protect disconnected hosts until they report a replacement. Collection
  prunes the catalog and archive files on startup and every 15 minutes. Failed uploads
  leave a local marker so orphaned archives are removed on the next pass. Stale incoming
  package files and private UI staging directories are removed after a day.
- The global event journal has finite age/count/byte bounds. Durable subscriptions protect
  unacknowledged events and may apply backpressure. Other lagging consumers receive an explicit
  gap boundary and recover from authoritative snapshots; unique triggers must not be overwritten.
- Mutation receipts form a FIFO ring bounded by the [shared count and byte limits](../packages/contracts/src/limits.ts),
  with no guaranteed retention duration. New writes atomically evict the oldest receipts
  to fit their actual encoded size. Replays do not refresh retention. A retained identical
  receipt resolves before rechecking newer domain state or the eviction boundary. Eviction
  advances a durable, monotonic issue-time boundary: absent identities at or before it and
  foreign runtime epochs fail instead of creating fresh work. Failed writes roll back both
  their effects and eviction. Service heartbeats carry liveness;
  unchanged adoption observations must not repeatedly consume durable mutation receipts.
  DataCollector prunes fresh results on publication and idle histories every 15 minutes.
- Resolved diagnostics are retained for at most seven days and 10,000 identities, oldest
  observation first. Current, stale and unknown conditions are never removed for history
  pressure; their combined global admission bound is 4,096 identities. Repeated observations
  update the retained timestamp so age remains correct across restarts.
- One collection pass removes at most 500 eligible revisions per contract family. Remaining
  work advances on later passes; candidate selection is not repeated for every deletion batch.
- Hive enables SQLite incremental page reclamation for new databases. Existing databases
  convert once during exclusive startup when temporary space is sufficient; interrupted or
  deferred conversion preserves data and retries on restart. Normal retention returns freed
  pages to the filesystem in bounded batches without rewriting live databases.
  Conversion builds and verifies its compact copy on the data volume, flushes it and
  atomically replaces the stopped database. A partial copy never becomes authoritative.
- Use [SDK operation identities](../packages/contracts/src/operation-id.ts) and owner status
  for recovery. Receipt expiry, missing records and retention gaps never prove that an external
  effect did not happen. Domain owners retain unresolved work and its necessary evidence.
- Released technical history uses bounded FIFO rings with no minimum retention duration.
  Reading a receipt does not refresh its place. Count and encoded byte pressure may evict
  completed history immediately; active, uncertain and referenced work is not a victim.
  Admission and result reservations leave room to finish or acknowledge existing work.

  | Owner | History eligible for eviction | State that remains protected |
  | --- | --- | --- |
  | AgentManager | Known completed native operations | Pending/unknown effects and turn ownership until its completion is handled |
  | Secretary | Completed actions, released technical workflows and successful follow-ups | Unacknowledged captures, prepared writes, running/unknown work and active context dependencies |
  | ChatBridge | Completed operations, acknowledged offers, terminal input plumbing and confirmed resolved diagnostic notices | Queued inputs/notices, partial offers, pending transport delivery and uncertain effects |
  | TaskBoard | Released local operation results and completed workflow plumbing | Active gates, pending dependencies, current task revisions, unconfirmed deliveries and uncertain native effects |
  | WhatsApp transport | Finished input bodies, confirmed outgoing bodies and inactive delivered-turn caches | Pending/unknown messages, current-turn deduplication and partly delivered turns |
  | DataCollector | Completed operation results and old known terminal runs | Latest task status, executions/publications, unknown outcomes and prepared definition writes |
  | PhoneBridge | Explicitly released independent results and results of fully released known calls | Original uncertainty, call/epoch identity, Voice bindings and archival recovery evidence |
  | Host runtime | Superseded successful/rolled-back deployments, resolved failures and older completed bootstrap details | Live work, unresolved failures, current adoption, recent deployments, replay identities and exact rollback evidence |

  Opaque IDs require compact persistent replay guards after payload eviction. These guards
  are required current state, not retained result history: an evicted identity must never
  create fresh work, including after an automatic binary rollback. Timestamped journals
  use a monotonic eviction fence. A missing receipt is not evidence of an absent effect.
- Diagnostic histories, Phone call samples, UI/native output caches, app releases and package
  versions already have capacity eviction. Current credentials, subscriptions, leases,
  configurations, domain records and referenced revisions are authoritative state, not
  disposable technical history. Browser-push capacity defers new ingestion until pending
  deliveries drain; it does not overwrite the outbox.
- Collection bounds and periods live in code. Runtime reset is separate from collection and
  follows [deployment recovery](DEPLOYMENT.md).
