import { canonical } from "../../../packages/contracts/src/canonical.js";
import type { Json } from "../../../packages/contracts/src/canonical.js";
import {
  IvyError,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import type { Wire } from "../../../packages/contracts/src/generated.js";
import type { ResourceRef } from "../../../packages/contracts/src/types.js";
import { Registry } from "./registry.js";
import type { Target } from "./registry.js";
import { utc } from "./paging.js";
import { HiveStore } from "./store.js";
import type { Row, ServiceContext } from "./store.js";
import type { Page } from "../../../packages/contracts/src/types.js";

export interface InventoryEntry {
  nativeId: string;
  summary: Json;
  observedAt: string;
}
export type InventorySync = {
  namespace: string;
  kind: string;
  schemaVersion: string;
  snapshotRevision: number;
  entries: InventoryEntry[];
} & (
  | { mode: "snapshot"; expectedRevision?: never; removedNativeIds?: never }
  | { mode: "delta"; expectedRevision: number; removedNativeIds: string[] }
);
export type InventoryListInput = Target & {
  namespace?: string;
  kind?: string;
  sort?: "native-id" | "recency-desc";
  archived?: boolean;
  searchTerm?: string;
  limit?: number;
  cursor?: string;
};

export class Inventory {
  constructor(
    readonly store: HiveStore,
    readonly registry: Registry,
  ) {}
  sync(
    context: ServiceContext,
    params: InventorySync,
  ): { snapshotRevision: number; observedAt: string } {
    return this.store.transaction(() => {
      const node = this.registry.node(context.serviceNodeId);
      requireThat(
        node.synced,
        "service_not_ready",
        "Registry synchronization is required before inventory.",
      );
      const kind = this.registry
        .namespace(node.serviceNodeId, params.namespace)
        .inventoryKinds.find(
          (kind) =>
            kind.kind === params.kind && kind.version === params.schemaVersion,
        );
      requireThat(
        kind,
        "registry_invalid",
        "Inventory kind/version is not in the current registry.",
      );
      requireThat(
        new Set(params.entries.map((entry) => entry.nativeId)).size ===
          params.entries.length,
        "registry_invalid",
        "Inventory contains duplicate native IDs.",
      );
      for (const entry of params.entries) {
        utc(entry.observedAt);
        this.store.validators.validate(kind.summarySchema, entry.summary);
      }
      if (params.mode === "delta")
        requireThat(
          new Set(params.removedNativeIds).size ===
            params.removedNativeIds.length &&
            !params.entries.some((entry) =>
              params.removedNativeIds.includes(entry.nativeId),
            ),
          "registry_invalid",
          "Delta contains conflicting or duplicate resource IDs.",
        );
      const previous = this.store.get(
        "SELECT * FROM inventory_sets WHERE node_id=? AND namespace=? AND kind=?",
        node.serviceNodeId,
        params.namespace,
        params.kind,
      );
      const revisionConflict = (message: string): never => {
        const details: Wire.InventoryRevisionConflictDetails = {
          serviceNodeId: node.serviceNodeId,
          namespace: params.namespace,
          kind: params.kind,
          currentRevision: Number(previous?.["revision"] ?? 0),
        };
        throw new IvyError(
          "revision_conflict",
          message,
          "not_executed",
          details,
        );
      };
      if (
        previous &&
        params.mode === "snapshot" &&
        previous["revision"] === params.snapshotRevision
      ) {
        const current = this.store
          .all(
            "SELECT native_id,summary,observed_at FROM inventory WHERE node_id=? AND namespace=? AND kind=? ORDER BY native_id",
            node.serviceNodeId,
            params.namespace,
            params.kind,
          )
          .map((row) => ({
            nativeId: String(row["native_id"]),
            summary: JSON.parse(String(row["summary"])) as Json,
            observedAt: String(row["observed_at"]),
          }));
        const sorted = [...params.entries].sort((a, b) =>
          Buffer.compare(Buffer.from(a.nativeId), Buffer.from(b.nativeId)),
        );
        if (
          previous["schema_version"] !== params.schemaVersion ||
          canonical(current) !== canonical(sorted)
        )
          revisionConflict(
            "Snapshot revision already identifies different inventory.",
          );
        return {
          snapshotRevision: params.snapshotRevision,
          observedAt: String(previous["observed_at"]),
        };
      }
      if (previous && params.snapshotRevision <= Number(previous["revision"]))
        revisionConflict("Inventory revision must advance.");
      if (
        params.mode === "delta" &&
        (!previous ||
          previous["revision"] !== params.expectedRevision ||
          previous["schema_version"] !== params.schemaVersion)
      )
        revisionConflict(
          "Inventory delta has a gap; send a fresh complete snapshot.",
        );
      const observedAt = params.entries.reduce(
        (latest, entry) =>
          entry.observedAt > latest ? entry.observedAt : latest,
        params.entries[0]?.observedAt ?? new Date().toISOString(),
      );
      this.store.run(
        "INSERT INTO inventory_sets VALUES (?,?,?,?,?,?) ON CONFLICT(node_id,namespace,kind) DO UPDATE SET revision=excluded.revision,schema_version=excluded.schema_version,observed_at=excluded.observed_at",
        node.serviceNodeId,
        params.namespace,
        params.kind,
        params.snapshotRevision,
        params.schemaVersion,
        observedAt,
      );
      if (params.mode === "snapshot")
        this.store.run(
          "DELETE FROM inventory WHERE node_id=? AND namespace=? AND kind=?",
          node.serviceNodeId,
          params.namespace,
          params.kind,
        );
      else
        for (const id of params.removedNativeIds)
          this.store.run(
            "DELETE FROM inventory WHERE node_id=? AND namespace=? AND kind=? AND native_id=?",
            node.serviceNodeId,
            params.namespace,
            params.kind,
            id,
          );
      for (const entry of params.entries)
        this.store.run(
          "INSERT INTO inventory VALUES (?,?,?,?,?,?,?) ON CONFLICT(node_id,namespace,kind,native_id) DO UPDATE SET schema_version=excluded.schema_version,summary=excluded.summary,observed_at=excluded.observed_at",
          node.serviceNodeId,
          params.namespace,
          params.kind,
          entry.nativeId,
          params.schemaVersion,
          canonical(entry.summary),
          entry.observedAt,
        );
      this.registry.save({ ...node, lastObservationAt: observedAt }, false);
      this.store.invalidate("inventory/" + params.namespace + "/" + params.kind + "/" + node.serviceNodeId);
      return { snapshotRevision: params.snapshotRevision, observedAt };
    });
  }
  private item(row: Row) {
    const node = this.registry.node(String(row["node_id"]));
    const set = this.store.get(
      "SELECT revision FROM inventory_sets WHERE node_id=? AND namespace=? AND kind=?",
      row["node_id"]!,
      row["namespace"]!,
      row["kind"]!,
    )!;
    return {
      resourceRef: {
        serviceNodeId: String(row["node_id"]),
        namespace: String(row["namespace"]),
        kind: String(row["kind"]),
        nativeId: String(row["native_id"]),
      },
      schemaVersion: String(row["schema_version"]),
      summary: JSON.parse(String(row["summary"])) as Json,
      observedAt: String(row["observed_at"]),
      stale:
        node.stale ||
        !this.registry.eligible(node) ||
        Date.now() - Date.parse(String(row["observed_at"])) > 60_000,
      snapshotRevision: Number(set["revision"]),
    };
  }
  get(resourceRef: ResourceRef) {
    const row = this.store.get(
      "SELECT * FROM inventory WHERE node_id=? AND namespace=? AND kind=? AND native_id=?",
      resourceRef.serviceNodeId,
      resourceRef.namespace,
      resourceRef.kind,
      resourceRef.nativeId,
    );
    requireThat(row, "not_found", "Native inventory resource not found.");
    return this.item(row);
  }
  list(params: InventoryListInput): Page<ReturnType<Inventory["item"]>> {
    if (params.resourceRef) {
      requireThat(
        (!params.serviceNodeId ||
          params.serviceNodeId === params.resourceRef.serviceNodeId) &&
          (!params.namespace ||
            params.namespace === params.resourceRef.namespace) &&
          (!params.kind || params.kind === params.resourceRef.kind),
        "target_conflict",
        "Inventory selectors disagree.",
      );
    }
    const nodeId = params.serviceNodeId ?? params.resourceRef?.serviceNodeId;
    if (nodeId) {
      const node = this.registry.node(nodeId);
      requireThat(
        (!params.hostId || params.hostId === node.hostId) &&
          (!params.serviceName || params.serviceName === node.serviceName),
        "target_conflict",
        "Inventory owner selectors disagree.",
      );
    }
    const namespace = params.namespace ?? params.resourceRef?.namespace ?? null;
    const kind = params.kind ?? params.resourceRef?.kind ?? null;
    const nativeId = params.resourceRef?.nativeId ?? null;
    const selectors = [
      nodeId ?? null,
      nodeId ?? null,
      params.hostId ?? null,
      params.hostId ?? null,
      params.serviceName ?? null,
      params.serviceName ?? null,
      namespace,
      namespace,
      kind,
      kind,
      nativeId,
      nativeId,
    ] as const;
    const joins = `FROM inventory i
      JOIN service_nodes n ON i.node_id=n.id
      JOIN inventory_sets s ON s.node_id=i.node_id AND s.namespace=i.namespace AND s.kind=i.kind
      JOIN inventory_schemas k ON k.namespace=i.namespace AND k.kind=i.kind AND k.version=i.schema_version`;
    const selected = `(? IS NULL OR n.id=?) AND (? IS NULL OR n.host_id=?) AND (? IS NULL OR n.service_name=?)
      AND (? IS NULL OR i.namespace=?) AND (? IS NULL OR i.kind=?) AND (? IS NULL OR i.native_id=?)`;
    if (params.archived !== undefined)
      requireThat(
        !this.store.get(
          `SELECT 1 ${joins} WHERE ${selected}
      AND json_type(k.definition,'$.archivedPointer') IS NULL LIMIT 1`,
          ...selectors,
        ),
        "invalid_arguments",
        "Selected inventory kind does not declare archive semantics.",
      );
    if (params.searchTerm)
      requireThat(
        !this.store.get(
          `SELECT 1 ${joins} WHERE ${selected}
      AND COALESCE(json_array_length(k.definition,'$.searchPointers'),0)=0 LIMIT 1`,
          ...selectors,
        ),
        "invalid_arguments",
        "Selected inventory kind does not declare searchable fields.",
      );
    if (params.sort === "recency-desc")
      requireThat(
        !this.store.get(
          `SELECT 1 ${joins} WHERE ${selected}
      AND json_type(k.definition,'$.recencyPointer') IS NULL LIMIT 1`,
          ...selectors,
        ),
        "invalid_arguments",
        "Selected inventory kind does not declare recency semantics.",
      );

    const sets = this.store.all(
      `SELECT s.node_id,s.namespace,s.kind,s.revision,s.schema_version
      FROM inventory_sets s JOIN service_nodes n ON s.node_id=n.id WHERE
      (? IS NULL OR n.id=?) AND (? IS NULL OR n.host_id=?) AND (? IS NULL OR n.service_name=?)
      AND (? IS NULL OR s.namespace=?) AND (? IS NULL OR s.kind=?)
      ORDER BY s.node_id,s.namespace,s.kind`,
      nodeId ?? null,
      nodeId ?? null,
      params.hostId ?? null,
      params.hostId ?? null,
      params.serviceName ?? null,
      params.serviceName ?? null,
      namespace,
      namespace,
      kind,
      kind,
    );
    const { cursor: _cursor, limit: _limit, ...identityParams } = params;
    const identity = {
      method: "inventory.list",
      selectors: identityParams,
      sets: hashRows(sets),
    };
    const boundary = params.cursor
      ? this.store.readCursor(identity, params.cursor)
      : null;
    requireThat(
      boundary === null ||
        (Array.isArray(boundary) &&
          boundary.length === (params.sort === "recency-desc" ? 5 : 4) &&
          boundary.every((value, index) =>
            index === 0 && params.sort === "recency-desc"
              ? typeof value === "number"
              : typeof value === "string",
          )),
      "invalid_cursor",
      "Invalid inventory boundary.",
    );
    const parts = boundary as (string | number)[] | null;
    const recency = `ivy_recency(i.summary,json_extract(k.definition,'$.recencyPointer'))`;
    const tieAfter = `(i.kind COLLATE BINARY>? OR (i.kind=? AND (i.native_id COLLATE BINARY>? OR
      (i.native_id=? AND (i.namespace COLLATE BINARY>? OR (i.namespace=? AND i.node_id COLLATE BINARY>?))))))`;
    const after =
      params.sort === "recency-desc"
        ? `(? IS NULL OR ${recency}<? OR (${recency}=? AND ${tieAfter}))`
        : `(? IS NULL OR ${tieAfter})`;
    const boundaryParams =
      params.sort === "recency-desc"
        ? [
            parts?.[0] ?? null,
            parts?.[0] ?? 0,
            parts?.[0] ?? 0,
            parts?.[1] ?? "",
            parts?.[1] ?? "",
            parts?.[2] ?? "",
            parts?.[2] ?? "",
            parts?.[3] ?? "",
            parts?.[3] ?? "",
            parts?.[4] ?? "",
          ]
        : [
            parts?.[0] ?? null,
            parts?.[0] ?? "",
            parts?.[0] ?? "",
            parts?.[1] ?? "",
            parts?.[1] ?? "",
            parts?.[2] ?? "",
            parts?.[2] ?? "",
            parts?.[3] ?? "",
          ];
    const rows = this.store.all(
      `SELECT i.*,s.revision AS snapshot_revision,${params.sort === "recency-desc" ? recency : "0"} AS sort_value
      ${joins} WHERE ${selected}
      AND (? IS NULL OR (ivy_type(i.summary,json_extract(k.definition,'$.archivedPointer'))=1
        AND ivy_value(i.summary,json_extract(k.definition,'$.archivedPointer'))=?))
      AND (? IS NULL OR EXISTS (SELECT 1 FROM json_each(k.definition,'$.searchPointers') p
        WHERE ivy_contains(i.summary,p.value,?)=1))
      AND ${after} AND ivy_budget()
      ORDER BY ${params.sort === "recency-desc" ? recency + " DESC," : ""}
        i.kind COLLATE BINARY,i.native_id COLLATE BINARY,i.namespace COLLATE BINARY,i.node_id COLLATE BINARY
      LIMIT ?`,
      ...selectors,
      params.archived === undefined ? null : Number(params.archived),
      Number(params.archived ?? false),
      params.searchTerm ? params.searchTerm.toLocaleLowerCase() : null,
      params.searchTerm?.toLocaleLowerCase() ?? "",
      ...boundaryParams,
      (params.limit ?? 50) + 1,
    );
    const nodes = new Map(
      this.registry.nodes().map((node) => [node.serviceNodeId, node]),
    );
    const items: ReturnType<Inventory["item"]>[] = [];
    let bytes = 128,
      trimmed = false;
    for (const row of rows.slice(0, params.limit ?? 50)) {
      const node = nodes.get(String(row["node_id"]));
      requireThat(
        node,
        "storage_invalid",
        "Inventory item references an unknown Service Node.",
      );
      const item = {
        resourceRef: {
          serviceNodeId: String(row["node_id"]),
          namespace: String(row["namespace"]),
          kind: String(row["kind"]),
          nativeId: String(row["native_id"]),
        },
        schemaVersion: String(row["schema_version"]),
        summary: JSON.parse(String(row["summary"])) as Json,
        observedAt: String(row["observed_at"]),
        stale:
          node.stale ||
          !this.registry.eligible(node) ||
          Date.now() - Date.parse(String(row["observed_at"])) > 60_000,
        snapshotRevision: Number(row["snapshot_revision"]),
      };
      const size = Buffer.byteLength(canonical(item));
      if (bytes + size > 2 * 1024 * 1024 - 8192) {
        requireThat(
          items.length,
          "result_too_large",
          "An inventory entry exceeds the response limit.",
        );
        trimmed = true;
        break;
      }
      bytes += size;
      items.push(item);
    }
    const more = trimmed || rows.length > items.length;
    const lastRow = items.length ? rows[items.length - 1]! : null;
    const nextBoundary = lastRow
      ? params.sort === "recency-desc"
        ? [
            Number(lastRow["sort_value"]),
            String(lastRow["kind"]),
            String(lastRow["native_id"]),
            String(lastRow["namespace"]),
            String(lastRow["node_id"]),
          ]
        : [
            String(lastRow["kind"]),
            String(lastRow["native_id"]),
            String(lastRow["namespace"]),
            String(lastRow["node_id"]),
          ]
      : null;
    return {
      items,
      nextCursor:
        more && nextBoundary ? this.store.cursor(identity, nextBoundary) : null,
    };
  }
}

function hashRows(rows: Row[]): string {
  return canonical(
    rows.map((row) =>
      Object.fromEntries(
        Object.entries(row).sort(([left], [right]) =>
          left.localeCompare(right),
        ),
      ),
    ),
  );
}
