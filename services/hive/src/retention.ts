import {
  canonical,
  compareVersions,
  hashJson,
} from "../../../packages/contracts/src/canonical.js";
import type {
  DataContract,
  RetentionPolicy,
} from "../../../packages/contracts/src/types.js";
import { EVENT_MAX_COUNT, HiveStore } from "./store.js";

const DAY = 24 * 60 * 60 * 1000;
const EVENT_MAX_BYTES = 64 * 1024 * 1024;
const REVISION_BATCH_SIZE = 500;

interface CandidateSummary {
  objectCount: number;
  revisionCount: number;
  byteLength: number;
  eligibleObjects: number;
  eligibleRevisions: number;
  protectedRevisions: number;
}

export interface FamilyRetentionStatus {
  contractKey: string;
  policy: RetentionPolicy;
  policyHash: string;
  previewRequired: boolean;
  objectCount: number;
  revisionCount: number;
  byteLength: number;
  eligibleObjectCount: number;
  eligibleRevisionCount: number;
  protectedRevisionCount: number;
}
export interface RetentionStatus {
  checkedAt: string;
  nextPassAt: string;
  families: FamilyRetentionStatus[];
  lastSuccessAt: string | null;
  lastFailure: { at: string; message: string } | null;
  events: { count: number; byteLength: number; prunedThroughSequence: number };
  mutations: { count: number; byteLength: number; expiredBefore: number };
}

export class Retention {
  constructor(
    readonly store: HiveStore,
    private readonly diagnostics?: { collectDiagnostics(now: number): void },
  ) {}

  private contracts(): DataContract[] {
    const latest = new Map<string, DataContract>();
    for (const row of this.store.all("SELECT definition FROM contracts")) {
      const value = JSON.parse(String(row["definition"])) as DataContract,
        current = latest.get(value.key);
      if (!current || compareVersions(value.version, current.version) > 0)
        latest.set(value.key, value);
    }
    return [...latest.values()].sort((a, b) => a.key.localeCompare(b.key));
  }

  private objectCandidatePlan(
    contracts: DataContract[],
    now: number,
  ): (enabled?: ReadonlySet<string>) => Map<string, string> {
    const policies = new Map(
      contracts.map((contract) => [contract.key, contract.retention.objects]),
    );
    const rows = this.store.all(
      "SELECT id,parent_id,owner_object_id,contract_key,created_at,updated_at FROM objects ORDER BY id",
    );
    const initialCandidates = new Set<string>(),
      contractKeys = new Map<string, string>(),
      requirements = new Map<string, Set<string>>(),
      dependents = new Map<string, Set<string>>();
    const requireCandidate = (target: string, required: string) => {
      const requiredByTarget = requirements.get(target) ?? new Set<string>();
      requiredByTarget.add(required);
      requirements.set(target, requiredByTarget);
      const targets = dependents.get(required) ?? new Set<string>();
      targets.add(target);
      dependents.set(required, targets);
    };
    for (const row of rows) {
      contractKeys.set(String(row["id"]), String(row["contract_key"]));
      const policy = policies.get(String(row["contract_key"]));
      if (
        (policy?.mode === "owned" &&
          Date.parse(String(row["created_at"])) <= now - DAY) ||
        (policy?.mode === "expire" &&
          Date.parse(String(row["updated_at"])) <=
            now - policy.maximumAgeDays * DAY)
      )
        initialCandidates.add(String(row["id"]));
      if (row["parent_id"])
        requireCandidate(String(row["parent_id"]), String(row["id"]));
      if (row["owner_object_id"])
        requireCandidate(String(row["owner_object_id"]), String(row["id"]));
    }
    for (const row of this.store.all(
      "SELECT DISTINCT source_object_id,target_object_id FROM revision_references",
    ))
      requireCandidate(
        String(row["target_object_id"]),
        String(row["source_object_id"]),
      );
    return (enabled?: ReadonlySet<string>) => {
      const candidates = new Set(
          [...initialCandidates].filter(
            (objectId) => !enabled || enabled.has(contractKeys.get(objectId)!),
          ),
        ),
        queue = [...candidates].filter((objectId) =>
          [...(requirements.get(objectId) ?? [])].some(
            (required) => !candidates.has(required),
          ),
        );
      for (let index = 0; index < queue.length; index++) {
        const objectId = queue[index]!;
        if (!candidates.delete(objectId)) continue;
        for (const target of dependents.get(objectId) ?? [])
          if (candidates.has(target)) queue.push(target);
      }
      return new Map(
        [...candidates].map((objectId) => [
          objectId,
          contractKeys.get(objectId)!,
        ]),
      );
    };
  }

  private objectCandidates(
    contracts: DataContract[],
    now: number,
  ): Map<string, string> {
    return this.objectCandidatePlan(contracts, now)();
  }

  private candidates(
    contract: DataContract,
    now: number,
    objectCandidates: ReadonlyMap<string, string>,
  ): {
    objects: string[];
    revisions: { objectId: string; revision: number }[];
    protectedCount: number;
  } {
    const objects: string[] = [],
      revisions: { objectId: string; revision: number }[] = [];
    let protectedCount = 0;
    for (const object of this.store.all(
      "SELECT id,current_revision FROM objects WHERE contract_key=? ORDER BY id",
      contract.key,
    )) {
      const objectId = String(object["id"]),
        current = Number(object["current_revision"]);
      const rows = this.store.all(
        "SELECT revision,created_at FROM revisions WHERE object_id=? ORDER BY revision DESC",
        objectId,
      );
      if (objectCandidates.has(objectId)) {
        objects.push(objectId);
        continue;
      }
      for (const row of rows) {
        const revision = Number(row["revision"]);
        if (revision === current || contract.retention.revisions.mode === "all")
          continue;
        const eligible =
          contract.retention.revisions.mode === "current" ||
          (contract.retention.revisions.mode === "bounded" &&
            ((contract.retention.revisions.maximumCount !== undefined &&
              revision <=
                current - contract.retention.revisions.maximumCount) ||
              (contract.retention.revisions.maximumAgeDays !== undefined &&
                Date.parse(String(row["created_at"])) <=
                  now - contract.retention.revisions.maximumAgeDays * DAY)));
        if (!eligible) continue;
        if (
          this.store.get(
            "SELECT 1 FROM revision_references WHERE target_object_id=? AND target_revision=? LIMIT 1",
            objectId,
            revision,
          )
        )
          protectedCount++;
        else revisions.push({ objectId, revision });
      }
    }
    return { objects, revisions, protectedCount };
  }

  private storedSummary(
    row: Record<string, unknown> | undefined,
  ): CandidateSummary {
    if (!row?.["summary_json"])
      return {
        objectCount: 0,
        revisionCount: 0,
        byteLength: 0,
        eligibleObjects: 0,
        eligibleRevisions: 0,
        protectedRevisions: 0,
      };
    const value = JSON.parse(
      String(row["summary_json"]),
    ) as Partial<CandidateSummary>;
    const count = (selected: unknown) =>
      typeof selected === "number" &&
      Number.isSafeInteger(selected) &&
      selected >= 0
        ? selected
        : 0;
    return {
      objectCount: count(value.objectCount),
      revisionCount: count(value.revisionCount),
      byteLength: count(value.byteLength),
      eligibleObjects: count(value.eligibleObjects),
      eligibleRevisions: count(value.eligibleRevisions),
      protectedRevisions: count(value.protectedRevisions),
    };
  }

  private hasStoredUsage(row: Record<string, unknown> | undefined): boolean {
    if (!row?.["summary_json"]) return false;
    const value = JSON.parse(String(row["summary_json"])) as Record<
      string,
      unknown
    >;
    return ["objectCount", "revisionCount", "byteLength"].every(
      (key) =>
        typeof value[key] === "number" &&
        Number.isSafeInteger(value[key]) &&
        Number(value[key]) >= 0,
    );
  }

  private revisionBatch(
    contract: DataContract,
    now: number,
    limit = REVISION_BATCH_SIZE + 1,
  ): { objectId: string; revision: number; byteLength: number }[] {
    const policy = contract.retention.revisions;
    if (policy.mode === "all") return [];
    const eligible: string[] = [];
    const params: Array<string | number> = [contract.key];
    if (policy.mode === "current") eligible.push("1");
    else {
      if (policy.maximumCount !== undefined) {
        eligible.push("r.revision<=o.current_revision-?");
        params.push(policy.maximumCount);
      }
      if (policy.maximumAgeDays !== undefined) {
        eligible.push("r.created_at<=?");
        params.push(new Date(now - policy.maximumAgeDays * DAY).toISOString());
      }
    }
    params.push(limit);
    return this.store
      .all(
        `SELECT r.object_id,r.revision,r.byte_length FROM revisions r
         JOIN objects o ON o.id=r.object_id
         WHERE r.contract_key=? AND r.revision<>o.current_revision
           AND (${eligible.join(" OR ")})
           AND NOT EXISTS (
             SELECT 1 FROM revision_references x
             WHERE x.target_object_id=r.object_id AND x.target_revision=r.revision
           )
         LIMIT ?`,
        ...params,
      )
      .map((row) => ({
        objectId: String(row["object_id"]),
        revision: Number(row["revision"]),
        byteLength: Number(row["byte_length"]),
      }));
  }

  preview(now = Date.now()): RetentionStatus {
    return this.buildStatus(now, true);
  }

  status(now = Date.now()): RetentionStatus {
    return this.buildStatus(now, false);
  }

  private buildStatus(now: number, scanCandidates: boolean): RetentionStatus {
    const contracts = this.contracts(),
      objectCandidates = scanCandidates
        ? this.objectCandidates(contracts, now)
        : new Map<string, string>();
    const families = contracts.map((contract) => {
      const policyHash = hashJson(contract.retention),
        saved = this.store.get(
          "SELECT policy_hash,summary_json FROM retention_families WHERE contract_key=?",
          contract.key,
        );
      const candidates = scanCandidates
          ? this.candidates(contract, now, objectCandidates)
          : null,
        summary = this.storedSummary(saved),
        totals =
          scanCandidates || saved?.["policy_hash"] !== policyHash
            ? this.store.get(
                "SELECT COUNT(*) AS objects FROM objects WHERE contract_key=?",
                contract.key,
              )!
            : null;
      const revisions =
        scanCandidates || saved?.["policy_hash"] !== policyHash
          ? this.store.get(
              "SELECT COUNT(*) AS revisions,COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE contract_key=?",
              contract.key,
            )!
          : null;
      return {
        contractKey: contract.key,
        policy: contract.retention,
        policyHash,
        previewRequired: saved?.["policy_hash"] !== policyHash,
        objectCount: totals ? Number(totals["objects"]) : summary.objectCount,
        revisionCount: revisions
          ? Number(revisions["revisions"])
          : summary.revisionCount,
        byteLength: revisions ? Number(revisions["bytes"]) : summary.byteLength,
        eligibleObjectCount: candidates
          ? candidates.objects.length
          : saved?.["policy_hash"] === policyHash
            ? summary.eligibleObjects
            : 0,
        eligibleRevisionCount: candidates
          ? candidates.revisions.length
          : saved?.["policy_hash"] === policyHash
            ? summary.eligibleRevisions
            : 0,
        protectedRevisionCount: candidates
          ? candidates.protectedCount
          : saved?.["policy_hash"] === policyHash
            ? summary.protectedRevisions
            : 0,
      };
    });
    const events = this.store.get(
      "SELECT COUNT(*) AS count,COALESCE(SUM(length(CAST(payload AS BLOB))),0) AS bytes FROM events",
    )!;
    const failure = this.store.metadata("retention_last_failure");
    return {
      checkedAt: new Date(now).toISOString(),
      nextPassAt: new Date(now + 15 * 60_000).toISOString(),
      families,
      lastSuccessAt: this.store.metadata("retention_last_success") || null,
      lastFailure: failure
        ? (JSON.parse(failure) as { at: string; message: string })
        : null,
      events: {
        count: Number(events["count"]),
        byteLength: Number(events["bytes"]),
        prunedThroughSequence: Number(
          this.store.metadata("events_pruned_through") ?? 0,
        ),
      },
      mutations: {
        count: Number(
          this.store.get("SELECT COUNT(*) AS count FROM mutations")!["count"],
        ),
        byteLength: this.store.mutationReceiptBytes,
        expiredBefore: Number(
          this.store.metadata("mutations_expired_before") ?? 0,
        ),
      },
    };
  }

  collect(now = Date.now()): RetentionStatus {
    const contracts = this.contracts(),
      candidatePlan = this.objectCandidatePlan(contracts, now),
      previewObjects = candidatePlan(),
      enabled = new Set<string>();
    for (const contract of contracts) {
      this.store.transaction(() => {
        const policyHash = hashJson(contract.retention),
          saved = this.store.get(
            "SELECT policy_hash,summary_json FROM retention_families WHERE contract_key=?",
            contract.key,
          );
        let summary: CandidateSummary;
        if (saved?.["policy_hash"] === policyHash) {
          enabled.add(contract.key);
          let prior = this.storedSummary(saved);
          if (!this.hasStoredUsage(saved)) {
            const totals = this.store.get(
                "SELECT COUNT(*) AS objects FROM objects WHERE contract_key=?",
                contract.key,
              )!,
              revisions = this.store.get(
                "SELECT COUNT(*) AS revisions,COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE contract_key=?",
                contract.key,
              )!;
            prior = {
              ...prior,
              objectCount: Number(totals["objects"]),
              revisionCount: Number(revisions["revisions"]),
              byteLength: Number(revisions["bytes"]),
            };
          }
          const batch = this.revisionBatch(contract, now),
            revisions = batch.slice(0, REVISION_BATCH_SIZE);
          if (revisions.length) {
            for (const candidate of revisions)
              this.store.run(
                "DELETE FROM revisions WHERE object_id=? AND revision=?",
                candidate.objectId,
                candidate.revision,
              );
            this.store.invalidate(`objects/${contract.key}`);
            this.store.invalidate("system");
          }
          summary = {
            objectCount: prior.objectCount,
            revisionCount: Math.max(0, prior.revisionCount - revisions.length),
            byteLength: Math.max(
              0,
              prior.byteLength -
                revisions.reduce(
                  (total, candidate) => total + candidate.byteLength,
                  0,
                ),
            ),
            eligibleObjects: 0,
            eligibleRevisions: Math.max(
              batch.length > REVISION_BATCH_SIZE ? 1 : 0,
              prior.eligibleRevisions - revisions.length,
            ),
            protectedRevisions: prior.protectedRevisions,
          };
        } else {
          const preview = this.candidates(contract, now, previewObjects),
            totals = this.store.get(
              "SELECT COUNT(*) AS objects FROM objects WHERE contract_key=?",
              contract.key,
            )!,
            revisions = this.store.get(
              "SELECT COUNT(*) AS revisions,COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE contract_key=?",
              contract.key,
            )!;
          summary = {
            objectCount: Number(totals["objects"]),
            revisionCount: Number(revisions["revisions"]),
            byteLength: Number(revisions["bytes"]),
            eligibleObjects: preview.objects.length,
            eligibleRevisions: preview.revisions.length,
            protectedRevisions: preview.protectedCount,
          };
        }
        this.store.run(
          "INSERT INTO retention_families VALUES (?,?,1,?) ON CONFLICT(contract_key) DO UPDATE SET policy_hash=excluded.policy_hash,previewed=1,summary_json=excluded.summary_json",
          contract.key,
          policyHash,
          canonical({
            checkedAt: new Date(now).toISOString(),
            objectCount: summary.objectCount,
            revisionCount: summary.revisionCount,
            byteLength: summary.byteLength,
            eligibleObjects: summary.eligibleObjects,
            eligibleRevisions: summary.eligibleRevisions,
            protectedRevisions: summary.protectedRevisions,
          }),
        );
      });
    }
    const objects = new Set(candidatePlan(enabled).keys());
    if (objects.size)
      this.store.transaction(() => {
        const rows = [...objects]
          .map((objectId) =>
            this.store.get(
              "SELECT id,depth,owner_object_id,contract_key FROM objects WHERE id=?",
              objectId,
            )!,
          )
          .sort(
            (a, b) =>
              Number(Boolean(b["owner_object_id"])) -
                Number(Boolean(a["owner_object_id"])) ||
              Number(b["depth"]) - Number(a["depth"]),
          );
        for (const row of rows) {
          const usage = this.store.get(
            "SELECT COUNT(*) AS revisions,COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE object_id=?",
            row["id"]!,
          )!;
          this.store.adjustRetentionUsage(
            String(row["contract_key"]),
            -1,
            -Number(usage["revisions"]),
            -Number(usage["bytes"]),
          );
        }
        for (const objectId of objects)
          this.store.run(
            "DELETE FROM revision_references WHERE source_object_id=?",
            objectId,
          );
        for (const objectId of objects) {
          this.store.run("DELETE FROM object_fts WHERE object_id=?", objectId);
          this.store.run("DELETE FROM revisions WHERE object_id=?", objectId);
        }
        for (const row of rows)
          this.store.run("DELETE FROM objects WHERE id=?", row["id"]!);
        this.store.invalidate("objects");
        this.store.invalidate("system");
      });
    this.store.transaction(() => {
      this.collectEvents(now);
      this.collectMutations(now);
      this.collectUiReleases();
      this.diagnostics?.collectDiagnostics(now);
      this.store.setMetadata(
        "retention_last_success",
        new Date(now).toISOString(),
      );
      this.store.setMetadata("retention_last_failure", "");
    });
    return this.status(now);
  }

  recordFailure(error: unknown, now = Date.now()): void {
    const message = error instanceof Error ? error.message : String(error);
    this.store.setMetadata(
      "retention_last_failure",
      canonical({
        at: new Date(now).toISOString(),
        message: message.slice(0, 512),
      }),
    );
  }

  private collectUiReleases(): void {
    for (const app of this.store.all(
      "SELECT app_id,current_release_id,previous_release_id FROM apps",
    )) {
      this.store.run(
        "DELETE FROM app_releases WHERE app_id=? AND release_id IS NOT ? AND release_id IS NOT ?",
        String(app["app_id"]),
        app["current_release_id"] ?? null,
        app["previous_release_id"] ?? null,
      );
    }
  }

  private collectEvents(now: number): void {
    const pruned = Number(this.store.metadata("events_pruned_through") ?? 0);
    let boundary = pruned;
    const age = this.store.get(
      "SELECT MAX(sequence) AS sequence FROM events WHERE occurred_at<?",
      new Date(now - 7 * DAY).toISOString(),
    );
    boundary = Math.max(boundary, Number(age?.["sequence"] ?? 0));
    const count = Number(
      this.store.get("SELECT COUNT(*) AS count FROM events")!["count"],
    );
    if (count > EVENT_MAX_COUNT)
      boundary = Math.max(
        boundary,
        Number(
          this.store.get(
            "SELECT sequence FROM events ORDER BY sequence LIMIT 1 OFFSET ?",
            count - EVENT_MAX_COUNT - 1,
          )?.["sequence"] ?? 0,
        ),
      );
    let bytes = 0;
    for (const row of this.store.all(
      "SELECT sequence,length(CAST(payload AS BLOB)) AS bytes FROM events ORDER BY sequence DESC",
    )) {
      bytes += Number(row["bytes"]);
      if (bytes > EVENT_MAX_BYTES) {
        boundary = Math.max(boundary, Number(row["sequence"]));
        break;
      }
    }
    boundary = Math.min(
      boundary,
      this.store.durableEventBoundary() ?? Number.MAX_SAFE_INTEGER,
    );
    if (boundary <= pruned) return;
    this.store.run("DELETE FROM events WHERE sequence<=?", boundary);
    this.store.setMetadata("events_pruned_through", String(boundary));
    this.store.run(
      "UPDATE subscriptions SET batch_json=NULL WHERE acknowledged_sequence<?",
      boundary,
    );
  }

  private collectMutations(now: number): void {
    this.store.cleanupMutationReceipts(now);
  }
}
