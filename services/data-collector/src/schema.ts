import { z } from "zod";
import type { Wire } from "../../../packages/sdk/src/node.js";

export const retentionSchema = z
  .object({
    maximumCount: z.number().int().min(1).max(1000000),
    maximumAgeDays: z.number().positive().max(36500),
    maximumBytes: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
  })
  .strict();
export const defaultRetention = {
  maximumCount: 1000,
  maximumAgeDays: 7,
  maximumBytes: 100 * 1024 * 1024,
};
const slug = z.string().regex(/^[a-z0-9][a-z0-9_-]{0,63}$/);
const secretName = z.string().regex(/^[a-zA-Z][a-zA-Z0-9_]{0,127}$/);
export const mcpSchema = z
  .object({
    serviceNodeId: z.string().min(1).max(256),
    cwd: z.string().min(1).max(4096),
    server: z.string().min(1).max(256),
    tools: z.array(z.string().min(1).max(256)).min(1).max(100),
  })
  .strict()
  .refine(
    (value) => new Set(value.tools).size === value.tools.length,
    "MCP tool names must be unique.",
  );
export const taskSchema = z
  .object({
    id: slug,
    name: z.string().min(1).max(200),
    enabled: z.boolean(),
    intervalSeconds: z.number().int().min(0).max(31536000),
    timeoutSeconds: z.number().int().min(1).max(3600),
    memoryMb: z.number().int().min(32).max(2048),
    script: z.string().min(1).max(262144),
    dependencies: z.record(
      z.string().regex(/^(?:@[a-z0-9-]+\/)?[a-z0-9][a-z0-9._-]*$/),
      z.string().regex(/^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?$/),
    ),
    config: z.record(z.string(), z.json()),
    secretNames: z.array(secretName).max(50),
    mcp: mcpSchema.optional(),
    inputSecretName: secretName.nullable().optional(),
    allowUnauthenticatedInput: z.boolean().optional(),
    retention: retentionSchema,
  })
  .strict()
  .refine((task) => !(task.allowUnauthenticatedInput && task.inputSecretName), {
    message: "Choose unauthenticated input or an input secret, not both.",
    path: ["allowUnauthenticatedInput"],
  });
export const settingsSchema = z
  .object({
    rootName: z.string().min(1).max(200).default("DataCollector"),
    rootObjectId: z.string().min(1).optional(),
    retention: retentionSchema.default(defaultRetention),
    concurrency: z.number().int().min(1).max(16).default(2),
    maximumTasks: z.number().int().min(1).max(1000).default(100),
    maximumMemoryMb: z.number().int().min(32).max(2048).default(512),
    maximumTimeoutSeconds: z.number().int().min(1).max(3600).default(300),
    npmCliPath: z.string().min(1).optional(),
    secrets: z.record(secretName, z.string()).default({}),
    ingress: z
      .object({
        host: z.string().default("127.0.0.1"),
        port: z.number().int().min(1).max(65535),
        routes: z
          .record(z.string().regex(/^\/[a-zA-Z0-9/_-]*$/), slug)
          .optional(),
      })
      .strict()
      .optional(),
  })
  .strict();
export const outputSchema = z
  .object({
    data: z.json(),
    state: z.json().optional(),
    events: z
      .array(
        z
          .object({
            name: z.string().regex(/^[a-z0-9][a-z0-9_.-]{0,127}$/),
            payload: z.json(),
          })
          .strict(),
      )
      .max(100)
      .optional(),
  })
  .strict();
export const readSchema = z.discriminatedUnion("view", [
  z.object({ view: z.literal("tasks") }),
  z.object({ view: z.literal("task"), id: slug }),
  z.object({
    view: z.literal("result"),
    id: slug,
    revision: z.number().int().positive().optional(),
  }),
  z.object({
    view: z.literal("history"),
    id: slug,
    cursor: z.string().optional(),
  }),
  z.object({
    view: z.literal("operation"),
    operationId: z.string().min(1).max(256),
  }),
]);
const mutation = { operationId: z.string().min(1).max(256) };
export const updateSchema = z.discriminatedUnion("action", [
  z
    .object({
      action: z.literal("save"),
      ...mutation,
      expectedRevision: z.number().int().min(0),
      task: taskSchema,
    })
    .strict(),
  z
    .object({
      action: z.enum(["enable", "delete"]),
      ...mutation,
      id: slug,
      expectedRevision: z.number().int().positive(),
    })
    .strict(),
  z
    .object({
      action: z.literal("disable"),
      ...mutation,
      id: slug,
      expectedRevision: z.number().int().positive(),
      confirmedStoppedRunId: z.string().min(1).max(256).optional(),
    })
    .strict(),
  z
    .object({
      action: z.literal("run"),
      ...mutation,
      id: slug,
      input: z.json().optional(),
    })
    .strict(),
]);
export type Task = z.infer<typeof taskSchema>;
export type Settings = z.infer<typeof settingsSchema>;
export type Output = z.infer<typeof outputSchema>;
export type Retention = z.infer<typeof retentionSchema>;
export function effectiveRetention(
  task: Retention,
  maximum: Retention,
): Retention {
  return {
    maximumCount: Math.min(task.maximumCount, maximum.maximumCount),
    maximumAgeDays: Math.min(task.maximumAgeDays, maximum.maximumAgeDays),
    maximumBytes: Math.min(task.maximumBytes, maximum.maximumBytes),
  };
}
export function registry(): Wire.RegistrySync {
  const contracts: Wire.DataContract[] = ["root", "result"].map((name) => ({
    key: "data-collector/" + name,
    version: "1.0.0",
    owner: { kind: "service", serviceName: "data-collector" },
    mediaType: "application/json",
    retention: {
      objects: { mode: "retain" },
      revisions: { mode: name === "root" ? "current" : "all" },
    },
    jsonSchema: { type: "object" },
    specMarkdown:
      name === "root"
        ? "Stable DataCollector result root."
        : "Successful script output; current revision is the latest result. Retention is applied by the owning collector.",
  }));
  return {
    mcpPrefix: "data_collector",
    discoveryHint:
      "DataCollector runs isolated Node scripts on intervals or configured HTTP inputs and stores versioned results in Hive.",
    namespaces: [
      {
        namespace: "data-collector",
        description: "Script tasks and versioned collection results",
        guideMarkdown:
          "Use data_collector_read with view tasks, task, result, history or operation. Use data_collector_update to save a complete task with expectedRevision (0 for creation), enable, disable, delete or run. Retain operationId for retries. After outcome_unknown, verify on the host that the task's workers stopped, then disable with confirmedStoppedRunId matching lastRunId to unlock it without rerunning. Scripts export default async ({config,secrets,state,input,signal}) and return {data,state?,events?}. Each event including collector metadata must fit 4096 UTF-8 bytes. Dependencies require exact npm versions. Only assigned secrets reach a task; do not put credentials in config, output or state. Tasks can use intervalSeconds=0 for input/manual runs. HTTP input requires an enabled task with inputSecretName or explicit allowUnauthenticatedInput=true, never both. The service listens on its own configured ingress port. Read tasks to discover effective service limits.",
        tools: [
          ["read", readSchema, true],
          ["update", updateSchema, false],
        ].map(([name, schema, readOnly]) => ({
          namespace: "data-collector",
          name: name as string,
          interfaceVersion: "1.0.0",
          description: readOnly
            ? "Read collector tasks, results, history or an original operation."
            : "Save, enable, disable, delete or run a script task.",
          discovery: {
            mcp: { name: "data_collector_" + name, surface: "ivy" },
            keywords: ["collection", "scripts"],
          },
          inputSchema: toolSchema(schema as typeof readSchema),
          outputSchema: {},
          annotations: {
            readOnlyHint: readOnly as boolean,
            idempotentHint: true,
          },
        })),
        topics: [
          {
            topic: "data-collector.event",
            version: "1.0.0",
            title: "Collected event",
            description: "A collected script event, such as camera motion.",
            payloadSchema: { type: "object" },
            eventKinds: [
              {
                kind: "collected",
                title: "Collected event",
                description: "An event emitted by a task.",
              },
            ],
          },
        ],
        inventoryKinds: [],
      },
    ],
    contracts,
    requiredContracts: contracts.map((c) => ({
      key: c.key,
      readVersions: [c.version],
      writeVersions: [c.version],
    })),
  };
}
function toolSchema(schema: z.ZodType): Record<string, unknown> {
  // Hive's schema dialect leaves map-key validation to the service's Zod parser.
  const generated = JSON.parse(
    JSON.stringify(z.toJSONSchema(schema)),
  ) as Record<string, unknown>;
  const visit = (value: unknown) => {
    if (value && typeof value === "object") {
      if (!Array.isArray(value))
        delete (value as Record<string, unknown>)["propertyNames"];
      for (const child of Object.values(value)) visit(child);
    }
  };
  visit(generated);
  return generated;
}
