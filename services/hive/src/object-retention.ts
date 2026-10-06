import { requireThat } from "../../../packages/contracts/src/errors.js";
import type { ServiceContext } from "./store.js";
import { HiveStore } from "./store.js";

/** An owning service can shorten its own history without removing current or referenced data. */
export function pruneObjectRevisions(
  store: HiveStore,
  context: ServiceContext,
  params: {
    objectId: string;
    expectedRevision: number;
    mutationId: string;
    maximumCount: number;
    maximumAgeDays: number;
    maximumBytes: number;
  },
) {
  return store.mutate(context, "objects.pruneRevisions", params, () => {
    const object = store.get(
      "SELECT * FROM objects WHERE id=?",
      params.objectId,
    );
    requireThat(object, "not_found", "Result object does not exist.");
    requireThat(
      Number(object["current_revision"]) === params.expectedRevision,
      "revision_conflict",
      "Result changed before retention.",
    );
    const current = store.get(
      "SELECT contract_version FROM revisions WHERE object_id=? AND revision=?",
      params.objectId,
      params.expectedRevision,
    )!;
    const contract = store.contract(
      String(object["contract_key"]),
      String(current["contract_version"]),
    );
    const owner = store.get(
      "SELECT service_name,principal_id FROM service_nodes WHERE id=?",
      context.serviceNodeId,
    );
    requireThat(
      owner &&
        owner["principal_id"] === context.principalId &&
        contract.owner.kind === "service" &&
        owner["service_name"] === contract.owner.serviceName,
      "forbidden",
      "Only the current owning service can prune result history.",
    );
    // Exact byte totals remain a native SQLite aggregate. Do not materialize or
    // rank the full history just to select one deletion batch.
    const totalBytes = Number(
      store.get(
        "SELECT COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE object_id=?",
        params.objectId,
      )!["bytes"],
    );
    store.checkBudget();
    const countBoundary = Number(
      store.get(
        "SELECT revision FROM revisions WHERE object_id=? ORDER BY revision DESC LIMIT 1 OFFSET ?",
        params.objectId,
        params.maximumCount - 1,
      )?.["revision"] ?? 0,
    );
    let cutoff = Math.max(0, countBoundary - 1);
    if (totalBytes > params.maximumBytes) {
      // This ordered window streams only the potentially retained prefix and
      // stops at the first byte overflow. Older rows already exceed the count.
      const overflow = store.get(
        `SELECT revision FROM (
          SELECT revision,SUM(byte_length) OVER (ORDER BY revision DESC) AS bytes
          FROM revisions WHERE object_id=? AND revision>=? AND ivy_budget()
        ) WHERE bytes>? LIMIT 1`,
        params.objectId,
        countBoundary,
        params.maximumBytes,
      );
      cutoff = Math.max(cutoff, Number(overflow?.["revision"] ?? 0));
    }
    const before = new Date(
      Date.now() - params.maximumAgeDays * 86400000,
    ).toISOString();
    const batch = store.all(
      `SELECT revision,byte_length FROM revisions r
       WHERE object_id=? AND revision<? AND (revision<=? OR created_at<=?)
         AND ivy_budget() AND NOT EXISTS (
           SELECT 1 FROM revision_references
           WHERE target_object_id=r.object_id AND target_revision=r.revision
         ) ORDER BY revision LIMIT 500`,
      params.objectId,
      params.expectedRevision,
      cutoff,
      before,
    );
    // Count incoming pins rather than probing every historical revision again.
    const protectedCount = Number(
      store.get(
        `SELECT COUNT(DISTINCT x.target_revision) AS count FROM revision_references x
       JOIN revisions r ON r.object_id=x.target_object_id AND r.revision=x.target_revision
       WHERE x.target_object_id=? AND r.revision<? AND (r.revision<=? OR r.created_at<=?)`,
        params.objectId,
        params.expectedRevision,
        cutoff,
        before,
      )!["count"],
    );
    store.checkBudget();
    let removedBytes = 0;
    for (const row of batch) {
      store.checkBudget();
      store.run(
        "DELETE FROM revisions WHERE object_id=? AND revision=?",
        params.objectId,
        row["revision"]!,
      );
      removedBytes += Number(row["byte_length"]);
    }
    if (batch.length) {
      store.adjustRetentionUsage(
        String(object["contract_key"]),
        0,
        -batch.length,
        -removedBytes,
      );
      store.invalidate(`objects/${String(object["contract_key"])}`);
      store.invalidate("system");
    }
    return {
      value: {
        deleted: batch.length,
        protected: protectedCount,
        remainingBytes: totalBytes - removedBytes,
      },
    };
  });
}
