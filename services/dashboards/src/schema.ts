import { z } from "zod";
import type { Wire } from "../../../packages/sdk/src/node.js";

export const source = z.union([
  z.object({ objectId: z.string().min(1) }).strict(),
  z
    .object({
      contractKey: z.string().min(1),
      limit: z.number().int().min(1).max(100).default(50),
    })
    .strict(),
]);
// Existing revisions retain their published contract; new saves use 1.1.0.
const originalDefinition = z
  .object({
    title: z.string().trim().min(1).max(200),
    html: z.string().min(1).max(500_000),
    sources: z
      .object({})
      .catchall(source)
      .refine(
        (v) =>
          Object.keys(v).length <= 20 &&
          Object.keys(v).every((key) =>
            /^[a-zA-Z][a-zA-Z0-9_]{0,63}$/.test(key),
          ),
      ),
    refreshSeconds: z
      .number()
      .int()
      .min(0)
      .max(86400)
      .refine((v) => v === 0 || v >= 5)
      .default(5),
  })
  .strict();
const jsonValue: z.ZodType<Wire.Json> = z.lazy(() =>
  z.union([
    z.string(),
    z.number(),
    z.boolean(),
    z.null(),
    z.array(jsonValue),
    z.object({}).catchall(jsonValue),
  ]),
);
export const definition = originalDefinition.extend({
  metadata: z
    .object({})
    .catchall(jsonValue)
    .default({})
    .refine(
      (value) =>
        !Object.hasOwn(value, "refreshSeconds") &&
        encodeURIComponent(JSON.stringify(value)).length <= 8000,
      "Metadata must fit in 8000 encoded bytes; refreshSeconds uses the refresh setting.",
    ),
  imageToken: z
    .string()
    .regex(/^[A-Za-z0-9_-]{32,128}$/)
    .nullable()
    .default(null),
});
export type Dashboard = z.infer<typeof definition>;
export const outputOptions = z
  .object({
    width: z.number().int().min(100).max(4096).default(800),
    height: z.number().int().min(100).max(4096).default(600),
    colorMode: z.enum(["color", "grayscale", "monochrome"]).default("color"),
  })
  .strict()
  .refine((v) => v.width * v.height <= 8_000_000, "At most 8 million pixels.");
export type OutputOptions = z.infer<typeof outputOptions>;
export const inputs = {
  list: z
    .object({
      cursor: z.string().optional(),
      limit: z.number().int().min(1).max(100).default(50),
    })
    .strict(),
  read: z.object({ id: z.string().min(1) }).strict(),
  save: z
    .object({
      id: z.string().min(1).optional(),
      expectedRevision: z.number().int().positive().optional(),
      value: definition,
    })
    .strict()
    .refine((v) => !!v.id === !!v.expectedRevision),
  delete: z
    .object({
      id: z.string().min(1),
      expectedRevision: z.number().int().positive(),
    })
    .strict(),
  data: z.object({ id: z.string().min(1) }).strict(),
  render: outputOptions.safeExtend({ id: z.string().min(1) }),
  image: outputOptions.safeExtend({
    id: z.string().min(1),
    token: z.string().min(1).max(128),
  }),
};
const descriptions = {
  list: "List saved dashboards with IDs, revisions and frameless URLs.",
  read: "Read dashboard HTML, sources, refresh interval, metadata, image access token and URLs.",
  save: "Create or replace a dashboard. Updates require the revision from read. HTML uses window.dashboard.onUpdate(async ({data, output}) => {}). Sources are objectId or contractKey/limit. Refresh is 0 or at least 5 seconds. Optional metadata is a JSON object; refreshSeconds comes from the refresh setting. imageToken is null (disabled) or a random 32–128 character URL-safe token granting only this dashboard's PNG access.",
  delete: "Delete a dashboard at its current revision.",
  data: "Read fresh data for the named Hive sources of a dashboard.",
  render:
    "Render fresh dashboard data as PNG at width/height (default 800×600). colorMode: color, grayscale (Gray8) or monochrome (strict black/white). Returns base64 PNG and metadata including refreshSeconds. HTML receives output.mode=image and dimensions/colorMode; async onUpdate is awaited.",
  image:
    "Token-authorized PNG rendering for the public HTTP image endpoint. The token grants access to exactly one dashboard image.",
};
export function dashboardRegistry(): Wire.RegistrySync {
  const schema = (value: z.ZodType) =>
    JSON.parse(
      JSON.stringify(
        z.toJSONSchema(value, { target: "draft-2020-12", io: "input" }),
      ),
    ) as Record<string, unknown>;
  return {
    mcpPrefix: "dashboard",
    discoveryHint:
      "Dashboards stores agent-authored HTML dashboards with live Hive data and renders PNG images for displays.",
    namespaces: [
      {
        namespace: "dashboards",
        description: "HTML dashboards and images",
        guideMarkdown:
          "Use dashboard_list/read/save/delete for CRUD and dashboard_render for PNG images. Keep operationId for retries. HTML runs without Hive credentials or network access. Register dashboard.onUpdate(async ({data, output}) => {...}); data maps source names to object content (or arrays for contractKey sources). output contains mode (interactive/image), width, height and colorMode (color/grayscale/monochrome). Use inline CSS/JS and data URLs. Return a promise from onUpdate for asynchronous drawing. The dashboard refreshes every refreshSeconds (0 disables refresh, minimum 5). Frameless HTML URLs require the normal Hive login. Set imageToken to a random URL-safe token to enable imageUrl: a direct PNG URL requiring no Hive login. It accepts width, height and colorMode. Responses include X-Dashboard-Refresh-Seconds and URL-encoded JSON X-Dashboard-Metadata, including on 304. Additional metadata is editable on the dashboard; imageToken=null disables public image access.",
        tools: Object.entries(inputs).map(([name, input]) => ({
          namespace: "dashboards",
          name,
          interfaceVersion: "1.0.0",
          description: descriptions[name as keyof typeof descriptions],
          inputSchema: schema(input),
          outputSchema: { type: "object" },
          annotations: {
            readOnlyHint: !["save", "delete"].includes(name),
            idempotentHint: name !== "render",
          },
          ...(name === "data" || name === "image"
            ? {}
            : {
                discovery: {
                  mcp: { name: "dashboard_" + name, surface: "ivy" as const },
                },
              }),
        })),
        topics: [],
        inventoryKinds: [],
      },
    ],
    contracts: [
      {
        key: "dashboards/dashboard",
        version: "1.0.0",
        owner: { kind: "service", serviceName: "dashboards" },
        mediaType: "application/json",
        retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
        jsonSchema: schema(originalDefinition),
        specMarkdown:
          "Agent-authored HTML with named Hive data sources and a refresh interval.",
      },
      {
        key: "dashboards/dashboard",
        version: "1.1.0",
        owner: { kind: "service", serviceName: "dashboards" },
        mediaType: "application/json",
        retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
        jsonSchema: schema(definition),
        specMarkdown:
          "HTML dashboard with named Hive sources, refresh interval, JSON metadata and optional scoped PNG access token.",
      },
    ],
    requiredContracts: [
      {
        key: "dashboards/dashboard",
        readVersions: ["1.0.0", "1.1.0"],
        writeVersions: ["1.1.0"],
      },
    ],
  };
}
