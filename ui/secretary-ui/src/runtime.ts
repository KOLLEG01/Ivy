import definition from "../ui.json";
import type { Operation, Wire } from "../../../packages/sdk/src/client.js";
import type { Agent } from "../../../packages/sdk/src/client.js";
import { nativeRouteQuery } from "../../../packages/ui-client/src/native-route";
import {
  queryDocument,
  serviceTools,
} from "../../../packages/sdk/src/client.js";
import { route, uiRuntime } from "../../../packages/ui-client/src/runtime";
import type {
  Assignment,
  AssignmentSummary,
  Document,
  Execution,
  ExecutionSummary,
  Pin,
  SecretaryConfiguration,
  Workspace,
} from "./types";

export const { base, client, live } = uiRuntime(definition.metadata);
export { route };
export const german = navigator.language.toLowerCase().startsWith("de");
export const tr = (de: string, en: string) => (german ? de : en);
export const dateLabel = (value: string | null) =>
  value ? new Date(value).toLocaleString() : "—";

const definitions = {
  "secretary/configuration": null as unknown as SecretaryConfiguration,
  "secretary/assignment": null as unknown as Assignment,
  "secretary/execution": null as unknown as Execution,
};
const readableVersions = new Map(
  definition.requirements.contracts.map(({ key, readVersions }) => [
    key,
    readVersions,
  ]),
);
export async function readDocument<K extends keyof typeof definitions>(
  key: K,
  pin: string | Pin,
  root: string,
  signal?: AbortSignal,
): Promise<Document<(typeof definitions)[K]>> {
  const read: Operation.ObjectRead = await client.request(
    "objects.read",
    typeof pin === "string" ? { objectId: pin } : pin,
    signal ? { signal } : {},
  );
  return decodeDocument(key, read, root);
}

function decodeDocument<K extends keyof typeof definitions>(
  key: K,
  read: Operation.ObjectRead,
  root: string,
): Document<(typeof definitions)[K]> {
  if (
    read.object.contractKey !== key ||
    read.object.parentId !== root ||
    read.object.effectivelyArchived ||
    !readableVersions.get(key)?.includes(read.revision.contractVersion) ||
    read.revision.mediaType !== "application/json" ||
    read.content.encoding !== "json"
  )
    throw new Error(
      tr(
        "Dieser Secretary-Eintrag gehört nicht zum gewählten Arbeitsbereich oder braucht eine neuere Oberfläche.",
        "This Secretary record belongs to another workspace or needs a newer UI.",
      ),
    );
  return {
    pin: { objectId: read.object.id, revision: read.revision.revision },
    value: read.content.value as unknown as (typeof definitions)[K],
    name: read.object.name,
  };
}

export async function listDocuments<K extends keyof typeof definitions>(
  key: K,
  root: string,
  signal?: AbortSignal,
  limit = 100,
  cursor?: string,
): Promise<{
  items: Document<(typeof definitions)[K]>[];
  nextCursor: string | null;
}> {
  const page: Operation.ObjectsQueryResult = await client.request(
    "objects.query",
    {
      contractKey: key,
      where: { op: "eq", field: "object.parentId", value: root },
      orderBy: [{ field: "object.updatedAt", direction: "desc" }],
      includeContent: true,
      limit,
      ...(cursor ? { cursor } : {}),
    },
    signal ? { signal } : {},
  );
  return {
    items: page.items.map((item) =>
      decodeDocument(key, queryDocument(item), root),
    ),
    nextCursor: page.nextCursor,
  };
}

const tools = (node: string) =>
  serviceTools(client, node, [
    { namespace: "secretary", interfaceVersion: "1.0.0" },
  ]);

const assignmentFields = {
  assignmentId: "assignmentId",
  name: "name",
  enabled: "enabled",
  triggerKind: "trigger/kind",
  cadence: "trigger/cadence",
  intervalMinutes: "trigger/intervalMinutes",
  localTime: "trigger/localTime",
  topic: "trigger/topic",
};
const executionFields = {
  name: "assignmentSnapshot/name",
  triggerKind: "trigger/kind",
  occurredAt: "trigger/occurredAt",
  createdAt: "createdAt",
  completedAt: "completedAt",
  phase: "phase",
  errorCode: "errorCode",
  cleanupState: "cleanup/state",
  cleanupError: "cleanup/errorCode",
  deliveryState: "delivery/state",
  deliveryReason: "delivery/reason",
  voiceState: "voice/state",
  voiceReason: "voice/reason",
};
export const summaryPageKey = (
  key: "secretary/assignment" | "secretary/execution",
  root: string,
) =>
  "secretary:summary:" +
  JSON.stringify([
    key,
    root,
    key === "secretary/assignment" ? assignmentFields : executionFields,
  ]);
export async function listSummaries<
  K extends "secretary/assignment" | "secretary/execution",
>(
  key: K,
  root: string,
  signal: AbortSignal,
  limit: number,
  cursor?: string,
): Promise<{
  items: Document<
    K extends "secretary/assignment" ? AssignmentSummary : ExecutionSummary
  >[];
  nextCursor: string | null;
}> {
  const fields =
    key === "secretary/assignment" ? assignmentFields : executionFields;
  const page = await client.request(
    "objects.query",
    {
      contractKey: key,
      where: { op: "eq", field: "object.parentId", value: root },
      orderBy: [{ field: "object.updatedAt", direction: "desc" }],
      includeContent: false,
      // Do not filter versions: unsupported records must remain visible as errors.
      // Optional scalars absent in older revisions project to null.
      select: [
        "object.parentId",
        "object.effectivelyArchived",
        "object.name",
        ...Object.values(fields).map((field) => "data:/" + field),
      ],
      limit,
      ...(cursor ? { cursor } : {}),
    },
    { signal },
  );
  return {
    items: page.items.map((item) => {
      if (
        item.values["object.parentId"] !== root ||
        item.values["object.effectivelyArchived"] !== false ||
        !readableVersions.get(key)?.includes(item.contractVersion)
      )
        throw new Error(
          tr(
            "Dieser Secretary-Eintrag braucht eine neuere Oberfläche.",
            "This Secretary record needs a newer UI.",
          ),
        );
      const value = Object.fromEntries(
        Object.entries(fields).map(([name, field]) => [
          name,
          item.values["data:/" + field],
        ]),
      );
      return {
        pin: { objectId: item.objectId, revision: item.revision },
        name: item.values["object.name"] as string,
        value: value as unknown as K extends "secretary/assignment"
          ? AssignmentSummary
          : ExecutionSummary,
      };
    }),
    nextCursor: page.nextCursor,
  };
}

export interface TopicChoice {
  provider: Operation.Provider | null;
  namespace: string;
  definition: Wire.TopicDefinition;
}
export const topicChoiceKey = (choice: TopicChoice) =>
  [
    choice.provider?.node.serviceNodeId ?? "hive",
    choice.namespace,
    choice.definition.topic,
    choice.definition.version,
  ].join("\u001f");
export async function listTopics(signal?: AbortSignal): Promise<TopicChoice[]> {
  const items: TopicChoice[] = [];
  let cursor: string | undefined;
  do {
    const page = await client.request(
      "topics.list",
      { limit: 100, ...(cursor ? { cursor } : {}) },
      signal ? { signal } : {},
    );
    items.push(...page.items);
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  return items.sort(
    (left, right) =>
      left.definition.title.localeCompare(right.definition.title) ||
      (left.provider?.node.serviceName ?? "hive").localeCompare(
        right.provider?.node.serviceName ?? "hive",
      ),
  );
}

export const loadWorkspace = async (
  node: string,
  signal?: AbortSignal,
): Promise<Workspace> => {
  const binding = (await tools(node).call(
    "secretary.binding",
    {},
    undefined,
    signal ? { signal } : {},
  )) as unknown as {
    expectedScope: Workspace["scope"];
    observedAt: string;
  };
  const {
    items: [configuration],
  } = await listDocuments(
    "secretary/configuration",
    binding.expectedScope.rootObjectId,
    signal,
    1,
  );
  return {
    scope: binding.expectedScope,
    configuration: configuration?.pin ?? null,
    observedAt: binding.observedAt,
  };
};
export const agentTaskUrl = (
  node: string,
  threadId: string,
  backend?: Agent.BackendIdentity,
) =>
  new URL(
    "agents/#/task?" +
      new URLSearchParams(
        nativeRouteQuery(
          {
            serviceNodeId: node,
            namespace: backend?.backendId ?? "codex",
            backend,
          },
          { id: threadId },
        ),
      ).toString(),
    base,
  ).href;
