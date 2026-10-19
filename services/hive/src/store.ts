import { DatabaseSync, backup } from "node:sqlite";
import type { SQLInputValue, StatementSync } from "node:sqlite";
import {
  randomBytes,
  randomUUID,
  createHmac,
  createHash,
  timingSafeEqual,
} from "node:crypto";
import { mkdirSync, createReadStream } from "node:fs";
import { dirname } from "node:path";
import { openHiveStorage } from './storage-compaction.js';
import {
  canonical,
  digest,
  hashJson,
} from "../../../packages/contracts/src/canonical.js";
import type { Json } from "../../../packages/contracts/src/canonical.js";
import {
  IvyError,
  fail,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import {
  SchemaValidators,
  pointerParts,
} from "../../../packages/contracts/src/schema.js";
import type { DataContract } from "../../../packages/contracts/src/types.js";
import { parseOperationId } from "../../../packages/contracts/src/operation-id.js";
import {
  mutationReceiptCount,
  mutationReceiptJournalBytes,
  mutationReceiptResultBytes,
} from "../../../packages/contracts/src/limits.js";
import {
  schemaSql,
  immutableTables,
  STORAGE_FORMAT,
} from "./storage-schema.js";

export type Row = Record<string, string | number | null | Uint8Array>;
export const EVENT_MAX_COUNT = 10_000;
// Keep limited room for consumer configuration and notices while publishers wait.
// Execution and progress writes themselves never append events.
const SECRETARY_EVENT_RESERVE = 1_024;
export interface AuthContext {
  credentialDigest: string;
  sessionDigest?: string;
}
export interface AuthenticatedContext extends AuthContext {
  principalId: string;
  expiresAt?: number;
}
export interface Principal {
  principalId: string;
  credentialDigest: string;
}
export interface ServiceContext extends AuthenticatedContext {
  serviceNodeId: string;
  generation: number;
}
export interface LedgerResult<T> {
  value: T;
  kind?: "snapshot" | "contract" | "event";
  reference?: Json;
}

export class HiveStore {
  /** Transient UI invalidations, drained by the worker after committed work. */
  readonly changes = new Set<string>();
  invalidate(scope: string): void {
    this.changes.add(scope);
    if (this.changes.size > 64) {
      this.changes.clear();
      for (const kind of ["objects", "services", "inventory", "uis", "system"]) this.changes.add(kind);
    }
  }
  readonly db: DatabaseSync;
  readonly validators = new SchemaValidators();
  private cursorSecret = "";
  private deadline = Infinity;
  private scalarCache = new Map<string, unknown>();
  private readonly statements = new Map<string, StatementSync>();
  private readonly contracts = new Map<string, DataContract>();

  constructor(readonly filename: string) {
    if (filename !== ":memory:")
      mkdirSync(dirname(filename), { recursive: true });
    const opened = openHiveStorage(filename, {
      timeout: 1000,
      enableForeignKeyConstraints: true,
      enableDoubleQuotedStringLiterals: false,
    });
    this.db = opened.db;
    try {
      this.initialize();
      this.setMetadata('storage_compaction_pending', opened.pending ?? '');
    } catch (error) {
      if (this.db.isOpen) this.db.close();
      throw error;
    }
  }
  private initialize(): void {
    this.db.exec(
      "PRAGMA locking_mode=EXCLUSIVE; PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=1000;",
    );
    const format = Number(this.get("PRAGMA user_version")!["user_version"]);
    if (format !== 0 && format !== STORAGE_FORMAT) {
      this.db.close();
      fail(
        "unsupported_storage_format",
        "This binary cannot read the database format.",
      );
    }
    if (format === 0)
      this.transaction(() => {
        this.db.exec(schemaSql);
        for (const table of immutableTables)
          for (const verb of ["UPDATE", "DELETE"]) {
            this.db.exec(
              `CREATE TRIGGER ${table}_immutable_${verb.toLowerCase()} BEFORE ${verb} ON ${table} BEGIN SELECT RAISE(ABORT, 'immutable_record'); END;`,
            );
          }
        this.run(
          "INSERT INTO metadata VALUES (?, ?)",
          "cursor_secret",
          randomBytes(32).toString("hex"),
        );
        this.run(
          "INSERT INTO metadata VALUES (?, ?)",
          "runtime_epoch",
          randomUUID(),
        );
        this.run(
          "INSERT INTO metadata VALUES (?, ?)",
          "events_pruned_through",
          "0",
        );
        this.run(
          "INSERT INTO metadata VALUES (?, ?)",
          "mutations_expired_before",
          "0",
        );
        this.run(
          "INSERT INTO metadata VALUES (?, ?)",
          "mutation_receipt_bytes",
          "0",
        );
        this.run(
          "INSERT INTO metadata VALUES (?, ?)",
          "retention_last_success",
          "",
        );
        this.run(
          "INSERT INTO metadata VALUES (?, ?)",
          "retention_last_failure",
          "",
        );
        this.db.exec(`PRAGMA user_version=${STORAGE_FORMAT}`);
      });
    // The reference Hive predates durable event subscriptions. This additive
    // table leaves its existing event journal and subscription cursors intact.
    this.db.exec(`CREATE TABLE IF NOT EXISTS durable_subscriptions (
      node_id TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(node_id, name),
      FOREIGN KEY(node_id, name) REFERENCES subscriptions(node_id, name)
    ) STRICT`);
    this.db.exec("DROP INDEX IF EXISTS mutations_expiry");
    this.cursorSecret = String(
      this.get("SELECT value FROM metadata WHERE key=?", "cursor_secret")![
        "value"
      ],
    );
    this.transaction(() => {
      for (const row of this.all("SELECT id, status_json FROM service_nodes")) {
        const status = JSON.parse(String(row["status_json"])) as Record<
          string,
          unknown
        >;
        Object.assign(status, {
          connected: false,
          synced: false,
          ready: false,
        });
        this.run(
          "UPDATE service_nodes SET status_json=? WHERE id=?",
          canonical(status),
          row["id"]!,
        );
      }
      this.db.exec("UPDATE subscriptions SET batch_json=NULL");
    });
    this.db.function("ivy_budget", () => {
      this.checkBudget();
      return 1;
    });
    this.db.function("ivy_type", (content, pointer) => {
      const value = this.scalar(content, pointer);
      return value === null
        ? 9
        : typeof value === "boolean"
          ? 1
          : typeof value === "number"
            ? 2
            : 3;
    });
    this.db.function("ivy_value", (content, pointer) => {
      const value = this.scalar(content, pointer);
      return typeof value === "boolean" ? Number(value) : value;
    });
    this.db.function("ivy_is_null", (content, pointer) =>
      Number(this.queryValue(content, pointer) === null),
    );
    this.db.function("ivy_contains", (content, pointer, needle) => {
      const value = this.queryValue(content, pointer);
      return Number(
        typeof value === "string" &&
          typeof needle === "string" &&
          value.toLocaleLowerCase().includes(needle),
      );
    });
    this.db.function("ivy_recency", (content, pointer) => {
      const value = this.queryValue(content, pointer);
      return typeof value === "number" && Number.isSafeInteger(value)
        ? value
        : typeof value === "string" && Number.isFinite(Date.parse(value))
          ? Date.parse(value)
          : 0;
    });
    const payloadSchema = {
      type: "object",
      properties: {
        objectId: { type: "string" },
        revision: { type: "integer", minimum: 1 },
        operation: { enum: ["write", "move", "archive"] },
      },
      required: ["objectId", "revision", "operation"],
      additionalProperties: false,
    };
    const definition = canonical({
      topic: "hive.object.changed",
      version: "1.0.0",
      payloadSchema,
    });
    this.run(
      "INSERT OR IGNORE INTO topics VALUES (?, ?, ?, ?)",
      "hive.object.changed",
      "1.0.0",
      "hive",
      definition,
    );
    this.run(
      "INSERT OR IGNORE INTO topics VALUES (?, ?, ?, ?)",
      "hive.object.changed",
      "1.1.0",
      "hive",
      canonical({
        topic: "hive.object.changed",
        version: "1.1.0",
        payloadSchema: {
          ...payloadSchema,
          properties: {
            ...payloadSchema.properties,
            operation: { enum: ["write", "move", "archive", "delete"] },
          },
        },
      }),
    );
  }

  close(): void {
    this.statements.clear();
    this.db.close();
  }
  private statement(sql: string): StatementSync {
    let statement = this.statements.get(sql);
    if (!statement) {
      statement = this.db.prepare(sql);
      if (this.statements.size >= 256)
        this.statements.delete(this.statements.keys().next().value!);
      this.statements.set(sql, statement);
    }
    return statement;
  }
  get(sql: string, ...params: SQLInputValue[]): Row | undefined {
    return this.statement(sql).get(...params) as Row | undefined;
  }
  all(sql: string, ...params: SQLInputValue[]): Row[] {
    return this.statement(sql).all(...params) as Row[];
  }
  run(
    sql: string,
    ...params: SQLInputValue[]
  ): ReturnType<StatementSync["run"]> {
    return this.statement(sql).run(...params);
  }
  metadata(key: string): string | null {
    return (
      (this.get("SELECT value FROM metadata WHERE key=?", key)?.[
        "value"
      ] as string) ?? null
    );
  }
  setMetadata(key: string, value: string): void {
    this.run(
      "INSERT INTO metadata VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
      key,
      value,
    );
  }
  adjustRetentionUsage(
    contractKey: string,
    objectDelta: number,
    revisionDelta: number,
    byteDelta: number,
  ): void {
    this.run(
      `UPDATE retention_families SET summary_json=json_set(
        summary_json,
        '$.objectCount',max(0,COALESCE(json_extract(summary_json,'$.objectCount'),0)+CAST(? AS INTEGER)),
        '$.revisionCount',max(0,COALESCE(json_extract(summary_json,'$.revisionCount'),0)+CAST(? AS INTEGER)),
        '$.byteLength',max(0,COALESCE(json_extract(summary_json,'$.byteLength'),0)+CAST(? AS INTEGER))
      ) WHERE contract_key=?
        AND json_type(summary_json,'$.objectCount')='integer'
        AND json_type(summary_json,'$.revisionCount')='integer'
        AND json_type(summary_json,'$.byteLength')='integer'`,
      objectDelta,
      revisionDelta,
      byteDelta,
      contractKey,
    );
  }
  get runtimeEpoch(): string {
    return this.metadata("runtime_epoch")!;
  }
  get mutationReceiptBytes(): number {
    return Number(this.metadata("mutation_receipt_bytes") ?? 0);
  }
  cleanupMutationReceipts(reservedBytes = 0, reservedCount = 0): void {
    requireThat(
      reservedBytes <= mutationReceiptJournalBytes,
      "result_too_large",
      "Mutation receipt exceeds the journal byte limit.",
    );
    let count = Number(
        this.get("SELECT COUNT(*) AS count FROM mutations")!["count"],
      ),
      bytes = this.mutationReceiptBytes,
      expiredBefore = Number(this.metadata("mutations_expired_before") ?? 0);
    const maximumCount = mutationReceiptCount - reservedCount,
      maximumBytes = mutationReceiptJournalBytes - reservedBytes;
    if (count <= maximumCount && bytes <= maximumBytes) return;
    // Row order is commit order, including receipts with identical or out-of-order
    // issue timestamps. Replaying a receipt never moves it to the back of the ring.
    while (count > maximumCount || bytes > maximumBytes) {
      const oldest = this.all(
        "SELECT rowid AS receipt_rowid,issued_at_ms,receipt_bytes FROM mutations ORDER BY rowid LIMIT ?",
        bytes > maximumBytes ? 256 : Math.min(256, count - maximumCount),
      );
      requireThat(
        oldest.length,
        "storage_invalid",
        "Mutation receipt usage does not match stored receipts.",
      );
      let boundary = 0;
      for (const row of oldest) {
        boundary = Number(row["receipt_rowid"]);
        count--;
        bytes -= Number(row["receipt_bytes"]);
        expiredBefore = Math.max(expiredBefore, Number(row["issued_at_ms"]));
        if (count <= maximumCount && bytes <= maximumBytes) break;
      }
      this.run("DELETE FROM mutations WHERE rowid<=?", boundary);
    }
    this.setMetadata("mutation_receipt_bytes", String(bytes));
    this.setMetadata("mutations_expired_before", String(expiredBefore));
  }
  transaction<T>(work: () => T): T {
    const priorChanges = new Set(this.changes);
    this.db.exec("BEGIN IMMEDIATE");
    try {
      const result = work();
      this.db.exec("COMMIT");
      return result;
    } catch (error) {
      if (this.db.isTransaction) this.db.exec("ROLLBACK");
      this.changes.clear();
      for (const scope of priorChanges) this.changes.add(scope);
      throw error;
    }
  }
  withBudget<T>(work: () => T, milliseconds = 1000): T {
    this.deadline = performance.now() + milliseconds;
    this.scalarCache.clear();
    try {
      return work();
    } finally {
      this.deadline = Infinity;
      this.scalarCache.clear();
    }
  }
  checkBudget(): void {
    if (performance.now() > this.deadline)
      fail("limit_exceeded", "Database operation exceeded its work budget.");
  }
  private scalar(
    content: SQLInputValue,
    pointer: SQLInputValue,
  ): null | boolean | string | number {
    const value = this.queryValue(content, pointer);
    return typeof value === "boolean" ||
      typeof value === "string" ||
      typeof value === "number"
      ? value
      : null;
  }
  private queryValue(content: SQLInputValue, pointer: SQLInputValue): unknown {
    this.checkBudget();
    if (typeof content !== "string" || typeof pointer !== "string") return null;
    let root = this.scalarCache.get(content);
    if (root === undefined) {
      try {
        root = JSON.parse(content) as unknown;
      } catch {
        return null;
      }
      if (this.scalarCache.size >= 8) this.scalarCache.clear();
      this.scalarCache.set(content, root);
    }
    let value: unknown = root;
    for (const key of pointerParts(pointer)) {
      if (
        value === null ||
        typeof value !== "object" ||
        !Object.hasOwn(value, key)
      )
        return null;
      if (Array.isArray(value) && !/^(0|[1-9]\d*)$/.test(key)) return null;
      value = (value as Record<string, unknown>)[key];
    }
    return value;
  }

  /** Host-only administration; no network operation exposes this method. */
  configureCredentials(
    credentials: { principalId: string; digest: string }[],
  ): void {
    this.transaction(() => {
      this.db.exec("UPDATE credentials SET revoked=1");
      for (const credential of credentials) {
        requireThat(
          credential.principalId.length > 0 &&
            credential.principalId.length <= 200 &&
            /^sha256:[a-f0-9]{64}$/.test(credential.digest),
          "invalid_arguments",
          "Invalid principal credential configuration.",
        );
        this.run(
          "INSERT OR IGNORE INTO principals VALUES (?)",
          credential.principalId,
        );
        const existing = this.get(
          "SELECT principal_id FROM credentials WHERE digest=?",
          credential.digest,
        );
        requireThat(
          !existing || existing["principal_id"] === credential.principalId,
          "invalid_arguments",
          "A credential cannot change its principal.",
        );
        this.run(
          "INSERT INTO credentials VALUES (?, ?, 0) ON CONFLICT(digest) DO UPDATE SET revoked=0",
          credential.digest,
          credential.principalId,
        );
      }
      const configured = new Set(
        credentials.map((credential) => credential.principalId),
      );
      for (const row of this.all(
        "SELECT DISTINCT principal_id FROM service_nodes",
      )) {
        const principalId = String(row["principal_id"]);
        if (!configured.has(principalId)) {
          this.run(
            "DELETE FROM durable_subscriptions WHERE node_id IN (SELECT id FROM service_nodes WHERE principal_id=?)",
            principalId,
          );
          this.run(
            "DELETE FROM subscriptions WHERE node_id IN (SELECT id FROM service_nodes WHERE principal_id=?)",
            principalId,
          );
        }
      }
    });
  }
  authenticate(context: AuthContext): Principal {
    const row = this.get(
      "SELECT principal_id FROM credentials WHERE digest=? AND revoked=0",
      context.credentialDigest,
    );
    requireThat(
      row,
      "unauthenticated",
      "A current Hive credential is required.",
    );
    if (context.sessionDigest)
      requireThat(
        this.get(
          "SELECT digest FROM browser_sessions WHERE digest=? AND credential_digest=?",
          context.sessionDigest,
          context.credentialDigest,
        ),
        "unauthenticated",
        "Browser session has expired.",
      );
    return {
      principalId: String(row["principal_id"]),
      credentialDigest: context.credentialDigest,
    };
  }
  mutate<T>(
    context: AuthenticatedContext,
    method: string,
    params: { mutationId: string; [key: string]: unknown },
    work: (principal: Principal) => LedgerResult<T>,
  ): T {
    return this.transaction(() => {
      const principal = {
        principalId: context.principalId,
        credentialDigest: context.credentialDigest,
      };
      const { mutationId, ...arguments_ } = params;
      const identity = parseOperationId(mutationId),
        now = Date.now();
      requireThat(
        identity.runtimeEpoch === this.runtimeEpoch,
        "mutation_expired",
        "Mutation identity belongs to another runtime.",
      );
      const requestHash = hashJson({ method, arguments: arguments_ });
      const prior = this.get(
        "SELECT * FROM mutations WHERE principal_id=? AND mutation_id=?",
        principal.principalId,
        mutationId,
      );
      if (prior) {
        requireThat(
          prior["request_hash"] === requestHash,
          "mutation_conflict",
          "The operation identity was already used with different arguments.",
        );
        const reference = JSON.parse(String(prior["result_json"])) as Record<
          string,
          unknown
        >;
        if (prior["result_kind"] === "contract")
          return this.contract(
            String(reference["key"]),
            String(reference["version"]),
          ) as T;
        return reference as T;
      }
      // Retained outcomes remain replayable even when an eviction has fenced
      // their issue time. An absent old identity must never become fresh work.
      requireThat(
        identity.issuedAtUnixMs <= now + 60_000 &&
          identity.issuedAtUnixMs >
            Number(this.metadata("mutations_expired_before") ?? 0),
        "mutation_expired",
        "Mutation identity is in the future or predates the receipt ring's replay boundary.",
      );
      const result = work(principal);
      const receipt = canonical(
        result.reference ?? result.value,
        mutationReceiptResultBytes,
      );
      const bytes =
        Buffer.byteLength(
          principal.principalId +
            mutationId +
            requestHash +
            (result.kind ?? "snapshot") +
            receipt,
        ) + 16;
      // Work, FIFO eviction, the replay boundary and the new receipt commit
      // together. A failed write leaves every previous outcome available.
      this.cleanupMutationReceipts(bytes, 1);
      this.run(
        "INSERT INTO mutations VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        principal.principalId,
        mutationId,
        requestHash,
        identity.issuedAtUnixMs,
        now,
        result.kind ?? "snapshot",
        receipt,
        bytes,
      );
      this.setMetadata(
        "mutation_receipt_bytes",
        String(this.mutationReceiptBytes + bytes),
      );
      return result.value;
    });
  }
  objectWriteReceipt(context: AuthenticatedContext, mutationId: string, expectedRequestHash: string): unknown | null {
    const row = this.get(
      "SELECT request_hash,result_kind,result_json FROM mutations WHERE principal_id=? AND mutation_id=?",
      context.principalId,
      mutationId,
    );
    if (!row) return null;
    requireThat(row["request_hash"] === expectedRequestHash && row["result_kind"] === "snapshot",
      "mutation_conflict", "The original mutation does not match this object write.");
    return JSON.parse(String(row["result_json"]));
  }
  contract(key: string, version: string): DataContract {
    const row = this.get(
      "SELECT definition FROM contracts WHERE key=? AND version=?",
      key,
      version,
    );
    requireThat(
      row,
      "contract_not_found",
      "The exact Data Contract is not registered.",
    );
    // Key by stored bytes so a rolled-back registration cannot poison the cache.
    const encoded = String(row["definition"]);
    let definition = this.contracts.get(encoded);
    if (!definition) {
      definition = JSON.parse(encoded) as DataContract;
      const freeze = (value: unknown): void => {
        if (!value || typeof value !== "object") return;
        for (const child of Object.values(value)) freeze(child);
        Object.freeze(value);
      };
      freeze(definition);
      if (this.contracts.size >= 256)
        this.contracts.delete(this.contracts.keys().next().value!);
      this.contracts.set(encoded, definition);
    }
    return definition;
  }
  eventFromRow(row: Row) {
    return {
      sequence: Number(row["sequence"]),
      topic: String(row["topic"]),
      topicVersion: String(row["topic_version"]),
      source: String(row["source"]),
      occurredAt: String(row["occurred_at"]),
      mutationId: String(row["mutation_id"]),
      payload: JSON.parse(String(row["payload"])) as Json,
    };
  }
  durableEventBoundary(): number | null {
    const value = this.get(`SELECT MIN(s.acknowledged_sequence) AS sequence FROM subscriptions s
      JOIN durable_subscriptions d ON d.node_id=s.node_id AND d.name=s.name`)?.["sequence"];
    return value === null || value === undefined ? null : Number(value);
  }
  pruneEventJournalForCapacity(): void {
    const count = Number(this.get("SELECT COUNT(*) AS count FROM events")!["count"]);
    if (count < EVENT_MAX_COUNT) return;
    const candidate = Number(this.get("SELECT sequence FROM events ORDER BY sequence LIMIT 1 OFFSET ?", count - (EVENT_MAX_COUNT - 1_024) - 1)?.["sequence"] ?? 0);
    const protectedThrough = this.durableEventBoundary();
    const boundary = Math.min(candidate, protectedThrough ?? Number.MAX_SAFE_INTEGER);
    const pruned = Number(this.metadata("events_pruned_through") ?? 0);
    if (boundary <= pruned) return;
    this.run("DELETE FROM events WHERE sequence<=?", boundary);
    this.setMetadata("events_pruned_through", String(boundary));
    this.run("UPDATE subscriptions SET batch_json=NULL WHERE acknowledged_sequence<?", boundary);
  }
  admitEvent(secretaryReserve = false): void {
    this.pruneEventJournalForCapacity();
    const count = Number(this.get("SELECT COUNT(*) AS count FROM events")!["count"]);
    requireThat(count < EVENT_MAX_COUNT + (secretaryReserve ? SECRETARY_EVENT_RESERVE : 0),
      "capacity_exceeded", "The durable Hive event journal is full; retry after Secretary acknowledges its backlog.");
  }
  appendCoreEvent(
    principal: Principal,
    objectId: string,
    revision: number,
    operation: string,
    mutationId: string,
    now: string,
    contractKey?: string,
  ): void {
    const key = contractKey ?? this.get("SELECT contract_key FROM objects WHERE id=?", objectId)?.["contract_key"];
    // Navigation and deletion can affect descendants of other contracts.
    this.invalidate(operation === "write" && key ? "objects/" + String(key) : "objects");
    // Internal consumer checkpoints must not replenish the journal they drain.
    if (["secretary/execution", "secretary/schedule-progress", "secretary/event-progress"].includes(String(key))) return;
    const secretary = Boolean(this.get("SELECT 1 FROM service_nodes WHERE principal_id=? AND service_name='secretary' LIMIT 1", principal.principalId));
    this.admitEvent(secretary);
    this.run(
      "INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES (?,?,?,?,?,?)",
      "hive.object.changed",
      operation === "delete" ? "1.1.0" : "1.0.0",
      `principal:${principal.principalId}`,
      now,
      mutationId,
      canonical({ objectId, revision, operation }),
    );
  }
  cursor(identity: unknown, boundary: Json): string {
    const body = Buffer.from(
      canonical({ query: hashJson(identity), boundary }),
    ).toString("base64url");
    return (
      body +
      "." +
      createHmac("sha256", this.cursorSecret).update(body).digest("base64url")
    );
  }
  readCursor(identity: unknown, cursor: string): Json {
    try {
      const [body, signature, extra] = cursor.split(".");
      requireThat(
        body &&
          signature &&
          !extra &&
          /^[A-Za-z0-9_-]+$/.test(body) &&
          /^[A-Za-z0-9_-]+$/.test(signature),
        "invalid_cursor",
        "Invalid cursor.",
      );
      const expected = createHmac("sha256", this.cursorSecret)
        .update(body)
        .digest();
      const supplied = Buffer.from(signature, "base64url");
      requireThat(
        expected.length === supplied.length &&
          timingSafeEqual(expected, supplied),
        "invalid_cursor",
        "Invalid cursor authentication.",
      );
      const data = JSON.parse(Buffer.from(body, "base64url").toString()) as {
        query: string;
        boundary: Json;
      };
      requireThat(
        data.query === hashJson(identity),
        "invalid_cursor",
        "Cursor belongs to a different query.",
      );
      return data.boundary;
    } catch (error) {
      if (error instanceof IvyError) throw error;
      return fail("invalid_cursor", "Invalid cursor.");
    }
  }
  async backupTo(
    destination: string,
    progress: () => void = () => undefined,
  ): Promise<{ pages: number; bytesHash: string }> {
    // SQLite's online snapshot includes WAL state without copying live database files.
    let copied = -1;
    const pages = await backup(this.db, destination, {
      progress: ({ totalPages, remainingPages }) => {
        const current = totalPages - remainingPages;
        if (current > copied) {
          copied = current;
          progress();
        }
      },
    });
    const hash = createHash("sha256");
    for await (const bytes of createReadStream(destination)) {
      hash.update(bytes);
      progress();
    }
    return { pages, bytesHash: "sha256:" + hash.digest("hex") };
  }
}
