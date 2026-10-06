import { randomUUID } from "node:crypto";
import {
  canonical,
  compareVersions,
  digest,
} from "../../../packages/contracts/src/canonical.js";
import { fail, requireThat } from "../../../packages/contracts/src/errors.js";
import type {
  Content,
  DataContract,
  ObjectMetadata,
  ObjectRead,
  ObjectWrite,
  ObjectWriteResult,
  Page,
  RevisionMetadata,
  RevisionReferences,
} from "../../../packages/contracts/src/types.js";
import { validateShared } from "../../../packages/contracts/src/core-validation.js";
import {
  binaryObjectContentBytes,
  jsonObjectContentBytes,
  textObjectContentBytes,
} from "../../../packages/contracts/src/limits.js";
import type { AuthenticatedContext, LedgerResult, Principal, Row, ServiceContext } from "./store.js";
import { HiveStore } from "./store.js";

interface Location {
  objectId?: string;
  path?: string;
}
export interface PageInput {
  limit?: number;
  cursor?: string;
}
const jsonMedia = (media: string): boolean =>
  /^application\/(?:json|[A-Za-z0-9!#$&^_.+-]+\+json)$/.test(media);
const scopeRootContract: DataContract = {
  key: "ivy-reference-rollout/root",
  version: "1.0.0",
  owner: { kind: "hive" },
  mediaType: "application/json",
  retention: { objects: { mode: "retain" }, revisions: { mode: "current" } },
  specMarkdown:
    "Empty stable scope anchor recreated by a full Reference Rollout reset.",
  jsonSchema: {
    type: "object",
    properties: { schemaVersion: { const: 1 } },
    required: ["schemaVersion"],
    additionalProperties: false,
  },
};

export class Objects {
  constructor(readonly store: HiveStore) {}

  /** Host-only reset bootstrap; no network operation exposes this method. */
  bootstrapScopeRoots(ids: string[], at = new Date().toISOString()): void {
    const roots = [...new Set(ids)].sort();
    for (const id of roots)
      requireThat(
        id.length > 0 && id.length <= 256,
        "invalid_arguments",
        "Invalid configured scope root Object ID.",
      );
    this.register(scopeRootContract, { kind: "hive" });
    const content = canonical({ schemaVersion: 1 }),
      contentHash = digest(content),
      byteLength = Buffer.byteLength(content, "utf8");
    this.store.transaction(() => {
      for (const id of roots) {
        const existing = this.store.get(
          "SELECT parent_id,owner_object_id,contract_key,current_revision,archived_at FROM objects WHERE id=?",
          id,
        );
        if (existing) {
          requireThat(
            existing["parent_id"] === null &&
              existing["owner_object_id"] === null &&
              existing["contract_key"] === scopeRootContract.key &&
              existing["current_revision"] === 1 &&
              existing["archived_at"] === null,
            "target_conflict",
            "Configured scope root ID is already used by another Object.",
          );
          continue;
        }
        const name = "Ivy scope " + digest(id).slice(7, 23);
        this.store.run(
          "INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) VALUES (?,NULL,NULL,?,?,?,NULL,1,?,1,NULL,0,?,?)",
          id,
          name,
          "/" + name,
          this.nextPosition(null),
          scopeRootContract.key,
          at,
          at,
        );
        this.store.run(
          "INSERT INTO revisions VALUES (?,?,?,?,?,?,?,?,?,?)",
          id,
          1,
          scopeRootContract.key,
          scopeRootContract.version,
          "json",
          content,
          contentHash,
          byteLength,
          at,
          "{}",
        );
        this.store.adjustRetentionUsage(
          scopeRootContract.key,
          1,
          1,
          byteLength,
        );
        this.updateSearch(id, name, {
          encoding: "json",
          value: { schemaVersion: 1 },
        });
      }
    });
  }

  /** Host-only reset bootstrap; services later register the same immutable definitions normally. */
  bootstrapServiceContracts(definitions: DataContract[]): void {
    for (const definition of definitions) {
      requireThat(
        definition.owner.kind === "service",
        "contract_owner_mismatch",
        "Reset bootstrap accepts only service-owned contracts.",
      );
      this.register(definition, definition.owner);
    }
  }

  register(
    definition: DataContract,
    owner: DataContract["owner"],
  ): DataContract {
    validateShared("DataContract", definition);
    compareVersions(definition.version, definition.version);
    requireThat(
      canonical(definition.owner) === canonical(owner),
      "contract_owner_mismatch",
      "Contract owner does not match the registered publisher.",
    );
    if (definition.jsonSchema !== undefined)
      this.store.validators.compile(definition.jsonSchema);
    const objectClass =
      definition.retention.objects.mode === "owned" ? "owned" : "retain";
    const family = this.store.get(
      "SELECT owner,media_type,object_mode FROM contract_families WHERE key=?",
      definition.key,
    );
    requireThat(
      !family ||
        (family["owner"] === canonical(owner) &&
          family["media_type"] === definition.mediaType &&
          family["object_mode"] === objectClass),
      "contract_owner_mismatch",
      "Contract family owner, media type and Object ownership class are immutable.",
    );
    const existing = this.store.get(
      "SELECT definition FROM contracts WHERE key=? AND version=?",
      definition.key,
      definition.version,
    );
    const encoded = canonical(definition);
    requireThat(
      !existing || existing["definition"] === encoded,
      "contract_definition_conflict",
      "An immutable contract version already has a different definition.",
    );
    if (!existing) {
      this.store.run(
        "INSERT OR IGNORE INTO contract_families VALUES (?,?,?,?)",
        definition.key,
        canonical(owner),
        definition.mediaType,
        objectClass,
      );
      this.store.run(
        "INSERT INTO contracts VALUES (?,?,?)",
        definition.key,
        definition.version,
        encoded,
      );
      this.store.invalidate("services");
    }
    return definition;
  }

  registerAgent(
    context: AuthenticatedContext,
    params: { definition: DataContract; mutationId: string },
  ): DataContract {
    return this.store.mutate(context, "contracts.register", params, () => ({
      value: this.register(params.definition, { kind: "agent" }),
      kind: "contract",
      reference: {
        key: params.definition.key,
        version: params.definition.version,
      },
    }));
  }
  getContract(key: string, version?: string): DataContract {
    if (version !== undefined) return this.store.contract(key, version);
    const definitions = this.store
      .all("SELECT definition FROM contracts WHERE key=?", key)
      .map((row) => JSON.parse(String(row["definition"])) as DataContract);
    requireThat(
      definitions.length,
      "contract_not_found",
      "Data Contract not found.",
    );
    definitions.sort((a, b) => compareVersions(b.version, a.version));
    return definitions[0]!;
  }

  decode(
    contract: DataContract,
    content: Content,
  ): { data: string | Uint8Array; contentHash: string; byteLength: number } {
    const encoding = jsonMedia(contract.mediaType)
      ? "json"
      : contract.mediaType.startsWith("text/")
        ? "text"
        : "base64";
    requireThat(
      content.encoding === encoding,
      "invalid_arguments",
      "Content encoding does not match the contract media type.",
    );
    let data: string | Uint8Array;
    if (content.encoding === "json") {
      data = canonical(content.value, jsonObjectContentBytes);
      requireThat(
        contract.jsonSchema !== undefined,
        "contract_not_found",
        "JSON contract has no schema.",
      );
      requireThat(
        this.store.validators.compile(contract.jsonSchema)(content.value),
        "invalid_arguments",
        "Content does not satisfy its exact contract.",
      );
    } else if (content.encoding === "text") {
      // JS strings with lone surrogates cannot be round-tripped as UTF-8 text.
      requireThat(
        content.value.isWellFormed(),
        "invalid_arguments",
        "Text must be valid Unicode.",
      );
      data = content.value;
    } else {
      const bytes = Buffer.from(content.value, "base64");
      requireThat(
        bytes.toString("base64") === content.value,
        "invalid_arguments",
        "Expected canonical base64 content.",
      );
      data = bytes;
    }
    const byteLength =
      typeof data === "string"
        ? Buffer.byteLength(data, "utf8")
        : data.byteLength;
    const maximum =
      encoding === "base64"
        ? binaryObjectContentBytes
        : encoding === "text"
          ? textObjectContentBytes
          : jsonObjectContentBytes;
    requireThat(
      byteLength <= maximum,
      "content_too_large",
      "Decoded content exceeds its storage limit.",
    );
    return { data, contentHash: digest(data), byteLength };
  }

  row(location: Location): Row {
    const row =
      location.objectId !== undefined
        ? this.store.get("SELECT * FROM objects WHERE id=?", location.objectId)
        : this.store.get("SELECT * FROM objects WHERE path=?", location.path!);
    requireThat(row, "not_found", "Object not found.");
    return row;
  }
  metadata(row: Row): ObjectMetadata {
    const revision = this.store.get(
      "SELECT contract_version FROM revisions WHERE object_id=? AND revision=?",
      row["id"]!,
      row["current_revision"]!,
    )!;
    return {
      id: String(row["id"]),
      parentId: row["parent_id"] as string | null,
      ownerObjectId: row["owner_object_id"] as string | null,
      name: String(row["name"]),
      path: String(row["path"]),
      position: Number(row["position"]),
      icon: row["icon"] as string | null,
      contractKey: String(row["contract_key"]),
      currentRevision: Number(row["current_revision"]),
      contractVersion: String(revision["contract_version"]),
      archivedAt: row["archived_at"] as string | null,
      effectivelyArchived: Boolean(row["effective_archive"]),
      createdAt: String(row["created_at"]),
      updatedAt: String(row["updated_at"]),
    };
  }
  revision(row: Row): RevisionMetadata {
    const definition = this.store.contract(
      String(row["contract_key"]),
      String(row["contract_version"]),
    );
    return {
      objectId: String(row["object_id"]),
      revision: Number(row["revision"]),
      contractVersion: String(row["contract_version"]),
      contentHash: String(row["content_hash"]),
      mediaType: definition.mediaType,
      byteLength: Number(row["byte_length"]),
      createdAt: String(row["created_at"]),
      references: JSON.parse(
        String(row["references_json"]),
      ) as RevisionReferences,
    };
  }
  stat(location: Location): ObjectMetadata {
    return this.metadata(this.row(location));
  }
  read(params: Location & { revision?: number }): ObjectRead {
    const object = this.stat(params);
    const row = this.store.get(
      "SELECT * FROM revisions WHERE object_id=? AND revision=?",
      object.id,
      params.revision ?? object.currentRevision,
    );
    if (
      !row &&
      params.revision !== undefined &&
      params.revision <= object.currentRevision
    )
      fail(
        "revision_pruned",
        "Object revision was pruned by its retention policy.",
      );
    requireThat(row, "not_found", "Object revision not found.");
    const content: Content =
      row["encoding"] === "base64"
        ? {
            encoding: "base64",
            value: Buffer.from(row["content"] as Uint8Array).toString("base64"),
          }
        : row["encoding"] === "json"
          ? { encoding: "json", value: JSON.parse(String(row["content"])) }
          : { encoding: "text", value: String(row["content"]) };
    return { object, revision: this.revision(row), content };
  }
  private name(value: string): string {
    requireThat(
      value.isWellFormed(),
      "invalid_arguments",
      "Object name must be valid Unicode.",
    );
    const name = value.normalize("NFC");
    requireThat(
      name.length > 0 &&
        name.length <= 255 &&
        !/[\/\\\0]/.test(name) &&
        name !== "." &&
        name !== "..",
      "invalid_arguments",
      "Invalid Object name.",
    );
    return name;
  }
  private parent(parentId: string | null): Row | null {
    return parentId === null ? null : this.row({ objectId: parentId });
  }
  private navigation(parent: Row | null, name: string) {
    const depth = parent === null ? 1 : Number(parent["depth"]) + 1;
    requireThat(
      depth <= 64,
      "limit_exceeded",
      "Object hierarchy exceeds its maximum depth.",
    );
    return {
      depth,
      path: parent === null ? "/" + name : String(parent["path"]) + "/" + name,
    };
  }
  private icon(value: string | null): string | null {
    if (value === null) return null;
    requireThat(
      value.isWellFormed(),
      "invalid_arguments",
      "Object icon must be valid Unicode.",
    );
    const icon = value.normalize("NFC").trim();
    requireThat(
      icon.length > 0 && icon.length <= 32 && !/[\r\n\0]/.test(icon),
      "invalid_arguments",
      "Object icon must be a short single-line value.",
    );
    return icon;
  }
  private nextPosition(parentId: string | null): number {
    const current = Number(
      this.store.get(
        "SELECT COALESCE(MAX(position),-1) AS position FROM objects WHERE parent_id IS ?",
        parentId,
      )!["position"],
    );
    requireThat(
      Number.isSafeInteger(current) && current < Number.MAX_SAFE_INTEGER,
      "limit_exceeded",
      "Object sibling position limit reached.",
    );
    return current + 1;
  }
  private normalizePositions(parentId: string | null): void {
    const rows = this.store.all(
      "SELECT id,position FROM objects WHERE parent_id IS ? ORDER BY position,id",
      parentId,
    );
    rows.forEach((row, position) => {
      if (Number(row["position"]) !== position)
        this.store.run(
          "UPDATE objects SET position=? WHERE id=?",
          position,
          row["id"]!,
        );
    });
  }
  private ensureName(
    parentId: string | null,
    name: string,
    excluding?: string,
  ): void {
    const existing = this.store.get(
      "SELECT id FROM objects WHERE parent_id IS ? AND name=?",
      parentId,
      name,
    );
    requireThat(
      !existing || existing["id"] === excluding,
      "revision_conflict",
      "An Object already uses that sibling name.",
    );
  }
  private updateSearch(id: string, name: string, content: Content): void {
    const searchId = this.row({ objectId: id })["search_id"]!;
    const text = content.encoding === "text" ? content.value : "";
    const previous = this.store.get(
      "SELECT name,text FROM object_fts WHERE rowid=?",
      searchId,
    );
    if (previous && previous["name"] === name && previous["text"] === text)
      return;
    this.store.run("DELETE FROM object_fts WHERE rowid=?", searchId);
    this.store.run(
      "INSERT INTO object_fts(rowid,object_id,name,text) VALUES (?,?,?,?)",
      searchId,
      id,
      name,
      text,
    );
  }

  private references(
    sourceObjectId: string,
    sourceRevision: number,
    references: RevisionReferences,
  ): string {
    const entries = Object.entries(references);
    requireThat(
      entries.length <= 256 &&
        Object.getPrototypeOf(references) === Object.prototype,
      "limit_exceeded",
      "A revision supports at most 256 exact references.",
    );
    for (const [name, target] of entries) {
      requireThat(
        /^[A-Za-z][A-Za-z0-9_.-]{0,127}$/.test(name) &&
          target &&
          typeof target === "object" &&
          !Array.isArray(target) &&
          Object.keys(target).length === 2 &&
          typeof target.objectId === "string" &&
          target.objectId.length > 0 &&
          target.objectId.length <= 256 &&
          Number.isSafeInteger(target.revision) &&
          target.revision > 0,
        "invalid_arguments",
        "Invalid exact revision reference.",
      );
      requireThat(
        target.objectId !== sourceObjectId,
        "invalid_arguments",
        "A revision cannot reference its own Object.",
      );
      requireThat(
        this.store.get(
          "SELECT 1 FROM revisions WHERE object_id=? AND revision=?",
          target.objectId,
          target.revision,
        ),
        "not_found",
        "Referenced Object revision is not committed.",
      );
    }
    return canonical(references, 128 * 1024);
  }

  write(context: AuthenticatedContext, params: ObjectWrite, preserveReferences = false): ObjectWriteResult {
    return this.writeAs(context, preserveReferences ? "objects.updateContent" : "objects.write", params, false, preserveReferences);
  }
  /** Hive-owned documents use the same immutable revision machinery without
   * allowing callers to manufacture another Object in the protected family. */
  writeHive(
    context: AuthenticatedContext,
    operation: string,
    params: ObjectWrite,
  ): ObjectWriteResult {
    return this.writeAs(context, operation, params, true);
  }
  private writeAs(
    context: AuthenticatedContext,
    operation: string,
    params: ObjectWrite,
    allowHive: boolean,
    preserveReferences = false,
  ): ObjectWriteResult {
    return this.store.mutate(context, operation, { ...params }, (principal) =>
      this.writeRevision(principal, params, allowHive, preserveReferences));
  }
  /** Resolve mutable inputs only after the original request's receipt is checked. */
  writeHivePrepared(
    context: AuthenticatedContext,
    operation: string,
    request: { mutationId: string; [key: string]: unknown },
    prepare: () => ObjectWrite,
  ): ObjectWriteResult {
    return this.store.mutate(context, operation, request, (principal) =>
      this.writeRevision(principal, prepare(), true));
  }
  private writeRevision(
    principal: Principal,
    params: ObjectWrite,
    allowHive: boolean,
    preserveReferences = false,
  ): LedgerResult<ObjectWriteResult> {
    const now = new Date().toISOString();
    const existing = params.create
      ? null
      : this.row({ objectId: params.objectId });
    if (existing) {
      requireThat(
        existing["current_revision"] === params.expectedRevision,
        "revision_conflict",
        "Object content changed; read the current revision.",
      );
      requireThat(params.expectedArchived === undefined || Boolean(existing["effective_archive"]) === params.expectedArchived,
        "revision_conflict", "Object archive state changed; read the current state.");
    }
    // Resolve retained references inside the receipt transaction, after replay
    // and CAS checks, so a Markdown-only edit neither drops links nor changes
    // the arguments of an original action retried after later page edits.
    const revisionReferences = preserveReferences && existing
      ? this.read({ objectId: String(existing["id"]), revision: params.expectedRevision! }).revision.references
      : params.references;
    const contractKey = params.create
      ? params.create.contractKey
      : String(existing!["contract_key"]);
    const contract = this.store.contract(contractKey, params.contractVersion);
    requireThat(
      allowHive || contract.owner.kind !== "hive",
      "forbidden",
      "Hive-owned Objects can only be changed through their dedicated operation.",
    );
    requireThat(
      !allowHive || contract.owner.kind === "hive",
      "contract_owner_mismatch",
      "Internal Hive writes require a Hive-owned contract.",
    );
    if (existing)
      requireThat(
        compareVersions(
          params.contractVersion,
          this.metadata(existing).contractVersion,
        ) >= 0,
        "contract_version_conflict",
        "Object contract version cannot be decreased.",
      );
    const decoded = this.decode(contract, params.content);
    const id = existing ? String(existing["id"]) : randomUUID();
    const revision = existing ? Number(existing["current_revision"]) + 1 : 1;
    requireThat(
      Number.isSafeInteger(revision),
      "limit_exceeded",
      "Revision limit reached.",
    );
    let name: string;
    if (params.create) {
      name = this.name(params.create.name);
      const parent = this.parent(params.create.parentId);
      requireThat(
        !parent ||
          this.store.get(
            "SELECT object_mode FROM contract_families WHERE key=?",
            parent["contract_key"]!,
          )?.["object_mode"] === "retain",
        "invalid_arguments",
        "Owned artifacts cannot be navigation parents.",
      );
      const nav = this.navigation(parent, name);
      this.ensureName(params.create.parentId, name);
      const ownerObjectId = params.create.ownerObjectId;
      if (contract.retention.objects.mode !== "owned")
        requireThat(
          ownerObjectId === null,
          "invalid_arguments",
          "Domain Objects cannot have a lifecycle owner.",
        );
      else {
        requireThat(
          ownerObjectId !== null && ownerObjectId !== id,
          "invalid_arguments",
          "Owned Objects require an existing lifecycle owner.",
        );
        const ownerRow = this.row({ objectId: ownerObjectId });
        requireThat(
          this.store.get(
            "SELECT object_mode FROM contract_families WHERE key=?",
            ownerRow["contract_key"]!,
          )?.["object_mode"] === "retain",
          "invalid_arguments",
          "The lifecycle owner must be a retained Object.",
        );
      }
      this.store.run(
        "INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,1,NULL,?,?,?)",
        id,
        params.create.parentId,
        ownerObjectId,
        name,
        nav.path,
        this.nextPosition(params.create.parentId),
        this.icon(params.create.icon ?? null),
        nav.depth,
        contractKey,
        parent?.["effective_archive"] ?? 0,
        now,
        now,
      );
    } else {
      name = params.name === undefined
        ? String(existing!["name"])
        : this.name(params.name);
      if (name !== existing!["name"])
        this.ensureName(existing!["parent_id"] as string | null, name, id);
      this.store.run(
        "UPDATE objects SET name=?,current_revision=?,updated_at=? WHERE id=?",
        name,
        revision,
        now,
        id,
      );
      if (name !== existing!["name"]) {
        this.refreshNavigation(id);
        // Descendant paths can belong to other contracts, just as with a move.
        this.store.invalidate("objects");
      }
    }
    const references = this.references(id, revision, revisionReferences);
    this.store.run(
      "INSERT INTO revisions VALUES (?,?,?,?,?,?,?,?,?,?)",
      id,
      revision,
      contractKey,
      params.contractVersion,
      params.content.encoding,
      decoded.data,
      decoded.contentHash,
      decoded.byteLength,
      now,
      references,
    );
    this.store.adjustRetentionUsage(
      contractKey,
      params.create ? 1 : 0,
      1,
      decoded.byteLength,
    );
    for (const [referenceName, target] of Object.entries(revisionReferences))
      this.store.run(
        "INSERT INTO revision_references VALUES (?,?,?,?,?)",
        id,
        revision,
        referenceName,
        target.objectId,
        target.revision,
      );
    this.updateSearch(id, name, params.content);
    this.store.appendCoreEvent(
      principal,
      id,
      revision,
      "write",
      params.mutationId,
      now,
    );
    const saved = this.store.get(
      "SELECT * FROM revisions WHERE object_id=? AND revision=?",
      id,
      revision,
    )!;
    return {
      value: {
        object: this.stat({ objectId: id }),
        revision: this.revision(saved),
      },
    };
  }

  private subtree(id: string): Row[] {
    return this.store.all(
      `WITH RECURSIVE descendants(id) AS (SELECT ? UNION ALL SELECT o.id FROM objects o JOIN descendants d ON o.parent_id=d.id)
      SELECT o.* FROM objects o JOIN descendants d ON o.id=d.id WHERE ivy_budget() ORDER BY o.depth,o.id`,
      id,
    );
  }
  move(
    context: AuthenticatedContext,
    params: {
      objectId: string;
      parentId: string | null;
      name: string;
      icon?: string | null;
      mutationId: string;
    },
  ): ObjectMetadata {
    return this.store.mutate(context, "objects.move", params, (principal) => {
      const rows = this.subtree(this.row(params)["id"] as string);
      const source = rows[0]!,
        previousParentId = source["parent_id"] as string | null,
        parent = this.parent(params.parentId),
        name = this.name(params.name);
      requireThat(
        !parent ||
          this.store.get(
            "SELECT object_mode FROM contract_families WHERE key=?",
            parent["contract_key"]!,
          )?.["object_mode"] === "retain",
        "invalid_arguments",
        "Owned artifacts cannot be navigation parents.",
      );
      requireThat(
        !rows.some((row) => row["id"] === params.parentId),
        "invalid_arguments",
        "An Object cannot be moved below itself.",
      );
      const nav = this.navigation(parent, name);
      const maximumDepth = rows.reduce(
        (maximum, row) => Math.max(maximum, Number(row["depth"])),
        0,
      );
      requireThat(
        maximumDepth + nav.depth - Number(rows[0]!["depth"]) <= 64,
        "limit_exceeded",
        "Move would exceed maximum hierarchy depth.",
      );
      this.ensureName(params.parentId, name, params.objectId);
      const now = new Date().toISOString();
      const parentChanged = previousParentId !== params.parentId;
      const position = parentChanged
        ? this.nextPosition(params.parentId)
        : Number(source["position"]);
      const icon = Object.hasOwn(params, "icon")
        ? this.icon(params.icon ?? null)
        : (source["icon"] as string | null);
      this.store.run(
        "UPDATE objects SET parent_id=?,name=?,position=?,icon=?,updated_at=? WHERE id=?",
        params.parentId,
        name,
        position,
        icon,
        now,
        params.objectId,
      );
      if (parentChanged) this.normalizePositions(previousParentId);
      this.refreshNavigation(params.objectId);
      this.updateSearch(
        params.objectId,
        name,
        this.read({ objectId: params.objectId }).content,
      );
      const object = this.stat(params);
      this.store.appendCoreEvent(
        principal,
        object.id,
        object.currentRevision,
        "move",
        params.mutationId,
        now,
      );
      return { value: object };
    });
  }
  reorder(
    context: AuthenticatedContext,
    params: {
      objectId: string;
      beforeObjectId: string | null;
      mutationId: string;
    },
  ): ObjectMetadata {
    return this.store.mutate(
      context,
      "objects.reorder",
      params,
      (principal) => {
        const source = this.row({ objectId: params.objectId });
        const parentId = source["parent_id"] as string | null;
        if (params.beforeObjectId !== null) {
          requireThat(
            params.beforeObjectId !== params.objectId,
            "invalid_arguments",
            "An Object cannot be ordered before itself.",
          );
          const before = this.row({ objectId: params.beforeObjectId });
          requireThat(
            before["parent_id"] === parentId,
            "invalid_arguments",
            "Objects can only be reordered within the same parent.",
          );
        }
        const ids = this.store
          .all(
            "SELECT id FROM objects WHERE parent_id IS ? ORDER BY position,id",
            parentId,
          )
          .map((row) => String(row["id"]))
          .filter((id) => id !== params.objectId);
        const target =
          params.beforeObjectId === null
            ? ids.length
            : ids.indexOf(params.beforeObjectId);
        requireThat(
          target >= 0,
          "not_found",
          "The target sibling is unavailable.",
        );
        ids.splice(target, 0, params.objectId);
        ids.forEach((id, position) =>
          this.store.run(
            "UPDATE objects SET position=? WHERE id=?",
            position,
            id,
          ),
        );
        const now = new Date().toISOString();
        this.store.run(
          "UPDATE objects SET updated_at=? WHERE id=?",
          now,
          params.objectId,
        );
        const object = this.stat({ objectId: params.objectId });
        this.store.appendCoreEvent(
          principal,
          object.id,
          object.currentRevision,
          "move",
          params.mutationId,
          now,
        );
        return { value: object };
      },
    );
  }
  private refreshNavigation(rootId: string): void {
    this.store.run(
      `WITH RECURSIVE navigation(id,path,depth,effective_archive) AS (
      SELECT root.id,
        CASE WHEN parent.id IS NULL THEN '/'||root.name ELSE parent.path||'/'||root.name END,
        COALESCE(parent.depth,0)+1,
        CASE WHEN root.archived_at IS NOT NULL OR COALESCE(parent.effective_archive,0)=1 THEN 1 ELSE 0 END
      FROM objects root LEFT JOIN objects parent ON parent.id=root.parent_id WHERE root.id=?
      UNION ALL
      SELECT child.id,navigation.path||'/'||child.name,navigation.depth+1,
        CASE WHEN child.archived_at IS NOT NULL OR navigation.effective_archive=1 THEN 1 ELSE 0 END
      FROM objects child JOIN navigation ON child.parent_id=navigation.id WHERE ivy_budget()
    ) UPDATE objects SET
      path=(SELECT path FROM navigation WHERE navigation.id=objects.id),
      depth=(SELECT depth FROM navigation WHERE navigation.id=objects.id),
      effective_archive=(SELECT effective_archive FROM navigation WHERE navigation.id=objects.id)
    WHERE id IN (SELECT id FROM navigation)`,
      rootId,
    );
  }
  archive(
    context: AuthenticatedContext,
    params: { objectId: string; archived: boolean; mutationId: string; expectedRevision?: number },
  ): ObjectMetadata {
    return this.store.mutate(
      context,
      "objects.archive",
      params,
      (principal) => {
        const object = this.row(params),
          now = new Date().toISOString();
        if (object['contract_key'] === 'task-board/task') {
          const serviceNodeId = (context as Partial<ServiceContext>).serviceNodeId;
          const owner = serviceNodeId ? this.store.get('SELECT service_name,principal_id FROM service_nodes WHERE id=?', serviceNodeId) : null;
          requireThat(owner?.['service_name'] === 'task-board' && owner['principal_id'] === principal.principalId,
            'task_board_workflow_required', 'Use task-board.archive to archive or restore a ticket together with its Codex tasks.');
        }
        requireThat(params.expectedRevision === undefined || Number(object["current_revision"]) === params.expectedRevision,
          "revision_conflict", "The document changed. Refresh it before archiving.");
        this.store.run(
          "UPDATE objects SET archived_at=?,updated_at=? WHERE id=?",
          params.archived ? (object["archived_at"] ?? now) : null,
          now,
          params.objectId,
        );
        this.refreshNavigation(params.objectId);
        const result = this.stat(params);
        this.store.appendCoreEvent(
          principal,
          params.objectId,
          result.currentRevision,
          "archive",
          params.mutationId,
          now,
        );
        return { value: result };
      },
    );
  }
  delete(
    context: AuthenticatedContext,
    params: { objectId: string; expectedRevision: number; mutationId: string },
  ): { deleted: boolean } {
    return this.store.mutate(context, "objects.delete", params, (principal) => {
      const root = this.row({ objectId: params.objectId });
      const contractKey = String(root["contract_key"]);
      requireThat(
        ["wiki/page", "wiki/attachment", "task-board/task", "dashboards/dashboard"].includes(contractKey),
        "invalid_arguments",
        "Permanent deletion is available only for Wiki pages, Wiki attachments, TaskBoard tasks and dashboards.",
      );
      requireThat(
        Number(root["current_revision"]) === params.expectedRevision,
        "revision_conflict",
        "The document changed. Refresh it before deleting.",
      );
      const selected = new Map<string, Row>();
      const taskParents = new Set<string | null>();
      const queue = [params.objectId];
      for (let index = 0; index < queue.length; index++) {
        this.store.checkBudget();
        const id = queue[index]!;
        if (selected.has(id)) continue;
        const row = this.row({ objectId: id });
        selected.set(id, row);
        if (row["contract_key"] === "task-board/task") {
          const current = this.store.get(
            "SELECT content FROM revisions WHERE object_id=? AND revision=?",
            id,
            Number(row["current_revision"]),
          )!;
          const task = JSON.parse(String(current["content"])) as {
            claim?: unknown;
            publication?: unknown;
            taskKey?: string;
          };
          requireThat(
            task.claim == null && task.publication == null,
            "target_conflict",
            "An active task or publication must finish before permanent deletion.",
          );
          taskParents.add(row["parent_id"] as string | null);
          // A selected workspace already brings all sibling workflow records.
          if (!selected.has(String(row["parent_id"]))) {
            for (const associated of this.store.all(
              `SELECT o.id FROM objects o JOIN revisions r
               ON r.object_id=o.id AND r.revision=o.current_revision
               WHERE o.contract_key IN ('task-board/run','task-board/result','task-board/review',
                 'task-board/history','task-board/delivery','task-board/comment-read-state')
                 AND o.parent_id IS ? AND r.encoding='json'
                 AND json_extract(r.content,'$.taskId')=? AND ivy_budget()`,
              row["parent_id"] as string | null,
              id,
            )) queue.push(String(associated["id"]));
            if (task.taskKey)
              for (const associated of this.store.all(
                `SELECT o.id FROM objects o JOIN revisions r
                 ON r.object_id=o.id AND r.revision=o.current_revision
                 WHERE o.contract_key='task-board/task-key-allocation'
                   AND o.parent_id IS ? AND r.encoding='json'
                   AND json_extract(r.content,'$.taskKey')=? AND ivy_budget()`,
                row["parent_id"] as string | null,
                task.taskKey,
              )) queue.push(String(associated["id"]));
          }
        }
        for (const dependent of this.store.all(
          "SELECT id FROM objects WHERE parent_id=? OR owner_object_id=?",
          id,
          id,
        )) queue.push(String(dependent["id"]));
      }
      for (const parentId of taskParents) {
        for (const dependency of this.store.all(
          `SELECT o.id,dependency.value AS target FROM objects o JOIN revisions r
           ON r.object_id=o.id AND r.revision=o.current_revision
           JOIN json_each(CASE WHEN r.encoding='json' THEN r.content ELSE '{}' END,
             '$.fields.dependencies') dependency
           WHERE o.contract_key='task-board/task' AND o.parent_id IS ? AND ivy_budget()`,
          parentId,
        )) requireThat(
          !selected.has(String(dependency["target"])) || selected.has(String(dependency["id"])),
          "target_conflict",
          "Another ticket depends on this ticket. Remove that dependency before deleting.",
        );
      }
      for (const id of selected.keys()) {
        const external = this.store.all(
          "SELECT DISTINCT source_object_id FROM revision_references WHERE target_object_id=?",
          id,
        ).some((row) => !selected.has(String(row["source_object_id"])));
        requireThat(
          !external,
          "target_conflict",
          "Another document references this content. Remove that reference before deleting.",
        );
      }
      for (const row of selected.values()) {
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
        this.store.run("DELETE FROM revision_references WHERE source_object_id=?", row["id"]!);
      }
      for (const row of selected.values()) {
        this.store.run("DELETE FROM object_fts WHERE object_id=?", row["id"]!);
        this.store.run("DELETE FROM revisions WHERE object_id=?", row["id"]!);
      }
      const rows = [...selected.values()].sort(
        (a, b) => Number(Boolean(b["owner_object_id"])) - Number(Boolean(a["owner_object_id"])) ||
          Number(b["depth"]) - Number(a["depth"]),
      );
      for (const row of rows) this.store.run("DELETE FROM objects WHERE id=?", row["id"]!);
      for (const parentId of new Set(rows.map((row) => row["parent_id"] as string | null)))
        if (parentId === null || !selected.has(parentId)) this.normalizePositions(parentId);
      this.store.appendCoreEvent(
        principal,
        params.objectId,
        params.expectedRevision,
        "delete",
        params.mutationId,
        new Date().toISOString(),
        contractKey,
      );
      return { value: { deleted: true } };
    });
  }
  list(
    params: PageInput & { parentId: string | null; includeArchived?: boolean },
  ): Page<ObjectMetadata> {
    if (params.parentId !== null) this.row({ objectId: params.parentId });
    const { cursor, ...identity } = params;
    // Siblings follow their navigation position; the ID only breaks ties while positions move.
    const boundary = cursor
      ? this.store.readCursor({ method: "objects.list", ...identity }, cursor)
      : [-1, ""];
    requireThat(
      Array.isArray(boundary) &&
        typeof boundary[0] === "number" &&
        typeof boundary[1] === "string",
      "invalid_cursor",
      "Invalid cursor.",
    );
    const [position, id] = boundary as [number, string];
    const limit = params.limit ?? 50;
    const rows = this.store.all(
      "SELECT * FROM objects WHERE parent_id IS ? AND (? OR effective_archive=0) AND (position>? OR (position=? AND id>?)) AND ivy_budget() ORDER BY position,id LIMIT ?",
      params.parentId,
      params.includeArchived ? 1 : 0,
      position,
      position,
      id,
      limit + 1,
    );
    const last = rows[limit - 1];
    return {
      items: rows.slice(0, limit).map((row) => this.metadata(row)),
      nextCursor:
        rows.length > limit
          ? this.store.cursor({ method: "objects.list", ...identity }, [
              Number(last!["position"]),
              String(last!["id"]),
            ])
          : null,
    };
  }
  history(params: PageInput & { objectId: string }): Page<RevisionMetadata> {
    const object = this.metadata(this.row(params));
    const { cursor, ...identity } = params,
      limit = params.limit ?? 50;
    const after = cursor
      ? Number(
          this.store.readCursor(
            { method: "objects.history", ...identity },
            cursor,
          ),
        )
      : null;
    const rows = this.store.all(
      "SELECT object_id,revision,contract_key,contract_version,content_hash,byte_length,created_at,references_json FROM revisions WHERE object_id=? AND (? IS NULL OR revision<?) ORDER BY revision DESC LIMIT ?",
      params.objectId,
      after,
      after,
      limit + 1,
    );
    const retained = Number(
      this.store.get(
        "SELECT COUNT(*) AS count FROM revisions WHERE object_id=?",
        params.objectId,
      )!["count"],
    );
    return {
      items: rows.slice(0, limit).map((row) => this.revision(row)),
      nextCursor:
        rows.length > limit
          ? this.store.cursor(
              { method: "objects.history", ...identity },
              Number(rows[limit - 1]!["revision"]),
            )
          : null,
      historyComplete: retained === object.currentRevision,
    };
  }
  tree(params: {
    rootId: string | null;
    depth?: number;
    limit?: number;
    includeArchived?: boolean;
  }): { items: ObjectMetadata[]; truncated: boolean } {
    if (params.rootId) this.row({ objectId: params.rootId });
    const limit = params.limit ?? 200,
      depth = params.depth ?? 3;
    const rows = this.store.all(
      `WITH RECURSIVE subtree(id,level) AS (
      SELECT id,1 FROM objects WHERE parent_id IS ? AND (? OR effective_archive=0)
      UNION ALL SELECT o.id,s.level+1 FROM objects o JOIN subtree s ON o.parent_id=s.id WHERE s.level<? AND (? OR o.effective_archive=0)
    ) SELECT o.*,s.level FROM objects o JOIN subtree s ON o.id=s.id WHERE ivy_budget() ORDER BY s.level,o.path LIMIT ?`,
      params.rootId,
      params.includeArchived ? 1 : 0,
      depth,
      params.includeArchived ? 1 : 0,
      limit + 1,
    );
    const items = rows.slice(0, limit);
    const deeper = items.some(
      (row) =>
        row["level"] === depth &&
        this.store.get(
          "SELECT 1 FROM objects WHERE parent_id=? AND (? OR effective_archive=0) LIMIT 1",
          row["id"]!,
          params.includeArchived ? 1 : 0,
        ),
    );
    return {
      items: items.map((row) => this.metadata(row)),
      truncated: rows.length > limit || deeper,
    };
  }
  search(
    params: PageInput & {
      text: string;
      contractKey?: string;
      rootId?: string;
      includeArchived?: boolean;
      excludeContractKey?: string;
    },
  ): Page<ObjectMetadata> {
    const { cursor, ...identity } = params,
      limit = params.limit ?? 50;
    const after = cursor
      ? String(
          this.store.readCursor(
            { method: "objects.search", ...identity },
            cursor,
          ),
        )
      : "";
    const root = params.rootId ? this.stat({ objectId: params.rootId }) : null;
    // Quoted tokens are data, never the FTS query language. Stable ID order avoids score cursor drift.
    const tokens = params.text.match(/[\p{L}\p{N}_]+/gu) ?? [];
    requireThat(
      tokens.length <= 32,
      "limit_exceeded",
      "Search supports at most 32 words.",
    );
    if (!tokens.length) return { items: [], nextCursor: null };
    const match = tokens
      .map((token) => '"' + token.replaceAll('"', '""') + '"')
      .join(" AND ");
    const rows = this.store.all(
      `SELECT o.* FROM object_fts f JOIN objects o ON o.id=f.object_id
      WHERE object_fts MATCH ? AND (? IS NULL OR o.contract_key=?) AND (? IS NULL OR o.contract_key<>?) AND (? OR o.effective_archive=0)
      AND (? IS NULL OR o.id=? OR substr(o.path,1,length(?)+1)=?||'/') AND o.id>? AND ivy_budget() ORDER BY o.id LIMIT ?`,
      match,
      params.contractKey ?? null,
      params.contractKey ?? null,
      params.excludeContractKey ?? null,
      params.excludeContractKey ?? null,
      params.includeArchived ? 1 : 0,
      root?.id ?? null,
      root?.id ?? null,
      root?.path ?? null,
      root?.path ?? null,
      after,
      limit + 1,
    );
    return {
      items: rows.slice(0, limit).map((row) => this.metadata(row)),
      nextCursor:
        rows.length > limit
          ? this.store.cursor(
              { method: "objects.search", ...identity },
              String(rows[limit - 1]!["id"]),
            )
          : null,
    };
  }
}
