import {
  canonical,
  digest,
} from "../../../packages/contracts/src/canonical.js";
import { fail, requireThat } from "../../../packages/contracts/src/errors.js";
import { validateHost } from "../../../packages/contracts/src/host-validation.js";
import { pointerParts } from "../../../packages/contracts/src/schema.js";
import type {
  Host,
  Operation,
  Wire,
} from "../../../packages/contracts/src/generated.js";
import type {
  DataContract,
  ObjectWrite,
} from "../../../packages/contracts/src/types.js";
import { Objects, type PageInput } from "./objects.js";
import type { AuthenticatedContext } from "./store.js";
import { HiveStore } from "./store.js";

interface SecretInstance extends Host.Instance {
  secretPaths?: string[];
}
interface SecretMarker {
  $ivySecret: { instanceId: string; path: string };
}
const pointer = (
  value: unknown,
  path: string,
): { present: boolean; value?: unknown } => {
  let current = value;
  for (const part of pointerParts(path)) {
    if (
      current === null ||
      typeof current !== "object" ||
      Array.isArray(current) ||
      !Object.hasOwn(current, part)
    )
      return { present: false };
    current = (current as Record<string, unknown>)[part];
  }
  return { present: true, value: current };
};
const setPointer = (
  value: unknown,
  path: string,
  replacement: unknown,
): void => {
  const parts = pointerParts(path);
  let current = value as Record<string, unknown>;
  for (const part of parts.slice(0, -1))
    current = current[part] as Record<string, unknown>;
  current[parts.at(-1)!] = replacement;
};
const marker = (value: unknown): SecretMarker["$ivySecret"] | null => {
  if (
    value === null ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    Object.keys(value).length !== 1
  )
    return null;
  const identity = (value as Record<string, unknown>)["$ivySecret"];
  if (
    identity === null ||
    typeof identity !== "object" ||
    Array.isArray(identity) ||
    Object.keys(identity).length !== 2
  )
    return null;
  const record = identity as Record<string, unknown>;
  return typeof record["instanceId"] === "string" &&
    typeof record["path"] === "string"
    ? (record as unknown as SecretMarker["$ivySecret"])
    : null;
};
const escapePointer = (value: string) =>
  value.replace(/~/g, "~0").replace(/\//g, "~1");
function markers(
  value: unknown,
  path = "",
): { path: string; identity: SecretMarker["$ivySecret"] }[] {
  const identity = marker(value);
  if (identity) return [{ path, identity }];
  if (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.hasOwn(value, "$ivySecret")
  )
    return fail(
      "invalid_arguments",
      "A configured-secret marker is malformed.",
    );
  if (Array.isArray(value))
    return value.flatMap((item, index) => markers(item, path + "/" + index));
  if (value !== null && typeof value === "object")
    return Object.entries(value).flatMap(([key, item]) =>
      markers(item, path + "/" + escapePointer(key)),
    );
  return [];
}
const paths = (instance: SecretInstance): string[] => [
  "/credential",
  ...(instance.secretPaths ?? []),
];

function redact(configuration: Host.HostConfig): Wire.Json {
  const projected = JSON.parse(
    canonical(configuration),
  ) as unknown as Host.HostConfig;
  for (const instance of projected.instances as SecretInstance[])
    for (const path of paths(instance)) {
      const selected = pointer(instance, path);
      if (selected.present)
        setPointer(instance, path, {
          $ivySecret: { instanceId: instance.instanceId, path },
        });
    }
  return projected as unknown as Wire.Json;
}

function restore(value: Wire.Json, current?: Host.HostConfig): Host.HostConfig {
  requireThat(
    value !== null && typeof value === "object" && !Array.isArray(value),
    "invalid_arguments",
    "Host configuration editor requires an object.",
  );
  const submitted = JSON.parse(canonical(value)) as unknown as Host.HostConfig;
  requireThat(
    Array.isArray(submitted.instances),
    "invalid_arguments",
    "Host configuration editor requires an instance array.",
  );
  requireThat(
    submitted.instances.every(
      (instance) =>
        instance !== null &&
        typeof instance === "object" &&
        !Array.isArray(instance),
    ),
    "invalid_arguments",
    "Host configuration editor instances must be objects.",
  );
  for (const instance of submitted.instances as SecretInstance[]) {
    requireThat(
      typeof instance.instanceId === "string" && instance.instanceId.length > 0,
      "invalid_arguments",
      "Host configuration editor instances require an identity.",
    );
    requireThat(
      instance.secretPaths === undefined ||
        (Array.isArray(instance.secretPaths) &&
          instance.secretPaths.every((path) => typeof path === "string")),
      "invalid_arguments",
      "Host configuration editor secretPaths must be string pointers.",
    );
    for (const path of instance.secretPaths ?? []) pointerParts(path);
  }
  const retained = new Map(
    (current?.instances ?? []).map((instance) => [
      instance.instanceId,
      instance as SecretInstance,
    ]),
  );
  for (const instance of submitted.instances as SecretInstance[]) {
    const previous = retained.get(instance.instanceId),
      allowed = new Set(previous ? paths(previous) : []),
      submittedPaths = new Set(paths(instance));
    for (const found of markers(instance)) {
      requireThat(
        previous &&
          found.path &&
          allowed.has(found.path) &&
          submittedPaths.has(found.path) &&
          found.identity.instanceId === instance.instanceId &&
          found.identity.path === found.path,
        "invalid_arguments",
        "A configured-secret marker must retain its original instance and declared setting path.",
      );
      const selected = pointer(previous, found.path);
      requireThat(
        selected.present,
        "invalid_arguments",
        "A configured-secret marker needs an existing value at its declared setting path.",
      );
      setPointer(instance, found.path, selected.value);
    }
  }
  return submitted;
}

const contract: DataContract = {
  key: "ivy/host-configuration",
  version: "1.0.0",
  owner: { kind: "hive" },
  mediaType: "application/json",
  retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
  specMarkdown:
    "Complete desired HostConfig JSON. Hive retains every accepted revision; the owning HostExecutor converges locally.",
  jsonSchema: {
    type: "object",
    properties: {
      schemaVersion: { const: 1 },
      hostId: { type: "string", minLength: 1, maxLength: 256 },
    },
    required: ["schemaVersion", "hostId"],
  },
};

export class Configurations {
  readonly objects: Objects;
  readonly contractKey = contract.key;
  constructor(
    readonly store: HiveStore,
    objects?: Objects,
  ) {
    this.objects = objects ?? new Objects(store);
    this.objects.register(contract, contract.owner);
  }
  private objectId(hostId: string): string | null {
    const rows = this.store.all(
      `SELECT o.id FROM objects o JOIN revisions r ON r.object_id=o.id AND r.revision=o.current_revision
      WHERE o.contract_key=? AND json_extract(r.content,'$.hostId')=? ORDER BY o.id LIMIT 2`,
      contract.key,
      hostId,
    );
    requireThat(
      rows.length <= 1,
      "storage_invalid",
      "Hive retained more than one current configuration for a host.",
    );
    return rows[0] ? String(rows[0]["id"]) : null;
  }
  private configuration(value: unknown, hostId: string): Host.HostConfig {
    validateHost("HostConfig", value);
    const config = value as Host.HostConfig;
    requireThat(
      config.hostId === hostId,
      "target_conflict",
      "Host configuration belongs to another host.",
    );
    requireThat(
      new Set(config.instances.map((instance) => instance.instanceId)).size ===
        config.instances.length &&
        new Set(config.instances.map((instance) => instance.serviceNodeId))
          .size === config.instances.length,
      "invalid_arguments",
      "Host instance and Service Node identities must be unique.",
    );
    for (const instance of config.instances as SecretInstance[]) {
      const secretPaths = instance.secretPaths ?? [];
      requireThat(
        new Set(secretPaths).size === secretPaths.length &&
          secretPaths.every(
            (path) =>
              path.startsWith("/settings/") &&
              !pointerParts(path).some((part) =>
                /^(?:0|[1-9][0-9]*)$/.test(part),
              ),
          ),
        "invalid_arguments",
        "Additional secret paths must be unique setting paths; protect arrays as a whole instead of addressing their elements.",
      );
    }
    return config;
  }
  get(hostId: string, revision?: number): Operation.HostConfiguration {
    const objectId = this.objectId(hostId);
    requireThat(objectId, "not_found", "Host configuration not found.");
    const read = this.objects.read({
      objectId,
      ...(revision === undefined ? {} : { revision }),
    });
    requireThat(
      read.content.encoding === "json",
      "storage_invalid",
      "Host configuration content is not JSON.",
    );
    const configuration = this.configuration(read.content.value, hostId);
    return {
      hostId,
      objectId,
      revision: read.revision.revision,
      contentHash: read.revision.contentHash,
      updatedAt: read.revision.createdAt,
      byteLength: read.revision.byteLength,
      configuration,
    };
  }
  list(): Operation.HostConfigurationSummary[] {
    return this.store
      .all(
        "SELECT id FROM objects WHERE contract_key=? AND effective_archive=0 ORDER BY id",
        contract.key,
      )
      .map((row) => {
        const read = this.objects.read({ objectId: String(row["id"]) });
        requireThat(
          read.content.encoding === "json",
          "storage_invalid",
          "Host configuration content is not JSON.",
        );
        const hostId = String(
          (read.content.value as Record<string, unknown>)["hostId"],
        );
        return this.get(hostId);
      })
      .sort((left, right) => left.hostId.localeCompare(right.hostId))
      .map(({ configuration: _configuration, ...summary }) => summary);
  }
  put(
    context: AuthenticatedContext,
    params: Operation.HostConfigurationsPutParams,
  ): Operation.HostConfiguration {
    const saved = this.objects.writeHivePrepared(
      context,
      "hostConfigurations.put",
      { ...params },
      () => this.prepareWrite(params),
    );
    return this.get(params.hostId, saved.revision.revision);
  }
  private prepareWrite(
    params: Operation.HostConfigurationsPutParams,
  ): ObjectWrite {
    const configuration = this.configuration(
      params.configuration,
      params.hostId,
    );
    const objectId = this.objectId(params.hostId);
    requireThat(
      objectId
        ? params.expectedRevision !== undefined
        : params.expectedRevision === undefined,
      "revision_conflict",
      objectId
        ? "Updating a host configuration requires its current revision."
        : "A new host configuration cannot name an existing revision.",
    );
    return {
      mutationId: params.mutationId,
      contractVersion: contract.version,
      references: {},
      content: {
        encoding: "json",
        value: JSON.parse(canonical(configuration)) as Wire.Json,
      },
      ...(objectId
        ? { objectId, expectedRevision: params.expectedRevision! }
        : {
            create: {
              contractKey: contract.key,
              parentId: null,
              ownerObjectId: null,
              name:
                "Ivy host configuration " + digest(params.hostId).slice(7, 23),
            },
          }),
    };
  }
  edit(hostId: string, revision?: number): Operation.HostConfigurationEditor {
    const { configuration, ...metadata } = this.get(hostId, revision);
    return { ...metadata, configuration: redact(configuration) };
  }
  projectRead(read: Operation.ObjectRead): Operation.ObjectRead {
    if (
      read.object.contractKey !== contract.key ||
      read.content.encoding !== "json"
    )
      return read;
    const hostId = String(
      (read.content.value as Record<string, Wire.Json>)["hostId"],
    );
    return {
      ...read,
      content: {
        encoding: "json",
        value: redact(this.configuration(read.content.value, hostId)),
      },
    };
  }
  isConfigurationContract(key: string): boolean {
    return key === contract.key;
  }
  save(
    context: AuthenticatedContext,
    params: Operation.HostConfigurationsSaveParams,
  ): Operation.HostConfigurationEditor {
    const saved = this.objects.writeHivePrepared(
      context,
      "hostConfigurations.save",
      { ...params },
      () => {
        const current = this.objectId(params.hostId)
          ? this.get(params.hostId).configuration
          : undefined;
        return this.prepareWrite({
          ...params,
          configuration: restore(params.configuration, current),
        });
      },
    );
    return this.edit(params.hostId, saved.revision.revision);
  }
  history(
    params: PageInput & { hostId: string },
  ): Operation.HostConfigurationsHistoryResult {
    const objectId = this.objectId(params.hostId);
    requireThat(objectId, "not_found", "Host configuration not found.");
    const result = this.objects.history({
      objectId,
      ...(params.limit === undefined ? {} : { limit: params.limit }),
      ...(params.cursor === undefined ? {} : { cursor: params.cursor }),
    });
    return { ...result, historyComplete: result.historyComplete ?? false };
  }
}
