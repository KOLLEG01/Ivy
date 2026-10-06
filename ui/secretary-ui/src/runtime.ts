import definition from "../ui.json";
import type { Operation, Wire } from "../../../packages/sdk/src/client.js";
import {
  newOperationId,
  queryDocument,
  serviceTools,
} from "../../../packages/sdk/src/client.js";
import { route, uiRuntime } from "../../../packages/ui-client/src/runtime";
import type {
  Assignment,
  Document,
  Execution,
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
export async function saveConfiguration(
  node: string,
  configuration: Pin,
  value: SecretaryConfiguration,
): Promise<Pin> {
  const operationId = await newOperationId(client);
  return (await tools(node).call(
    "secretary.saveConfiguration",
    { operationId, configuration, value } as unknown as Wire.Json,
    operationId,
  )) as unknown as Pin;
}
export async function saveAssignment(
  node: string,
  assignment: Pin | null,
  value: Assignment,
): Promise<Pin> {
  const operationId = await newOperationId(client);
  return (await tools(node).call(
    "secretary.saveAssignment",
    { operationId, assignment, value } as unknown as Wire.Json,
    operationId,
  )) as unknown as Pin;
}
export const agentTaskUrl = (node: string, threadId: string) =>
  new URL(
    "agents/#/task?node=" +
      encodeURIComponent(node) +
      "&id=" +
      encodeURIComponent(threadId),
    base,
  ).href;
