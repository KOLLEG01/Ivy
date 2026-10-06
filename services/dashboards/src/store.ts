import { IvyError, requireThat } from "../../../packages/sdk/src/node.js";
import type { RpcClient, Wire } from "../../../packages/sdk/src/node.js";
import { definition } from "./schema.js";
import type { Dashboard } from "./schema.js";
import { createHash, timingSafeEqual } from "node:crypto";

export class DashboardStore {
  constructor(
    readonly client: RpcClient,
    readonly root: string | null,
    readonly base: string,
    readonly node: string,
  ) {}
  url(id: string) {
    return new URL(
      "dashboards/#/view?node=" +
        encodeURIComponent(this.node) +
        "&id=" +
        encodeURIComponent(id),
      this.base,
    ).href;
  }
  imageUrl(id: string, token: string | null) {
    if (!token) return null;
    const url = new URL("api/v1/dashboards/image.png", this.base);
    url.search = new URLSearchParams({ node: this.node, id, token }).toString();
    return url.href;
  }
  async read(id: string) {
    const item = await this.client.request("objects.read", { objectId: id });
    requireThat(
      item.object.contractKey === "dashboards/dashboard" &&
        item.object.parentId === this.root &&
        !item.object.effectivelyArchived &&
        ["1.0.0", "1.1.0"].includes(item.revision.contractVersion) &&
        item.content.encoding === "json",
      "target_conflict",
      "Object is not an active dashboard in this workspace.",
    );
    const value = definition.parse(item.content.value);
    return {
      id,
      revision: item.revision.revision,
      value,
      url: this.url(id),
      imageUrl: this.imageUrl(id, value.imageToken),
    };
  }
  async image(id: string, token: string) {
    const row = await this.read(id);
    const hash = (value: string) => createHash("sha256").update(value).digest();
    requireThat(
      row.value.imageToken &&
        timingSafeEqual(hash(row.value.imageToken), hash(token)),
      "unauthenticated",
      "Invalid dashboard image token.",
    );
    return row;
  }
  async list(cursor?: string, limit = 50) {
    const result = await this.client.request("objects.query", {
      contractKey: "dashboards/dashboard",
      contractVersions: ["1.0.0", "1.1.0"],
      where: this.root
        ? { op: "eq", field: "object.parentId", value: this.root }
        : { op: "isNull", field: "object.parentId" },
      limit,
      ...(cursor ? { cursor } : {}),
    });
    const items: {
      id: string;
      revision: number;
      title: string;
      url: string;
      imageUrl: string | null;
    }[] = [];
    for (let offset = 0; offset < result.items.length; offset += 16)
      items.push(
        ...(await Promise.all(
          result.items.slice(offset, offset + 16).map(async (item) => {
            const row = await this.read(item.objectId);
            return {
              id: row.id,
              revision: row.revision,
              title: row.value.title,
              url: row.url,
              imageUrl: row.imageUrl,
            };
          }),
        )),
      );
    return { items, nextCursor: result.nextCursor };
  }
  async save(
    value: Dashboard,
    id: string | undefined,
    expectedRevision: number | undefined,
    operation: string,
  ) {
    if (id) await this.read(id);
    const result = await this.client.request("objects.write", {
      mutationId: operation,
      contractVersion: "1.1.0",
      references: {},
      content: { encoding: "json", value: value as Wire.Json },
      ...(id
        ? { objectId: id, expectedRevision: expectedRevision! }
        : {
            create: {
              name: "dashboard-" + operation.replaceAll(/[^a-zA-Z0-9-]/g, "-"),
              contractKey: "dashboards/dashboard",
              parentId: this.root,
              ownerObjectId: null,
            },
          }),
    });
    return {
      id: result.object.id,
      revision: result.revision.revision,
      url: this.url(result.object.id),
      imageUrl: this.imageUrl(result.object.id, value.imageToken),
    };
  }
  async remove(id: string, expectedRevision: number, operation: string) {
    try {
      await this.read(id);
    } catch (error) {
      if (!(error instanceof IvyError && error.code === "not_found"))
        throw error;
    }
    await this.client.request("objects.delete", {
      objectId: id,
      expectedRevision,
      mutationId: operation,
    });
    return { id, deleted: true };
  }
  async data(value: Dashboard) {
    const read = async (id: string): Promise<Wire.Json> => {
      const result = await this.client.request("objects.read", {
        objectId: id,
      });
      requireThat(
        !result.object.effectivelyArchived,
        "not_found",
        "A dashboard source is archived.",
      );
      // Host configuration contains service credentials and is not dashboard data.
      requireThat(
        result.object.contractKey !== "ivy/host-configuration",
        "forbidden",
        "System configuration is not a dashboard source.",
      );
      requireThat(
        result.content.encoding === "json" ||
          result.content.encoding === "text",
        "invalid_arguments",
        "Dashboard sources must be JSON or text.",
      );
      // A dashboard used as data must never leak its image access token.
      if (
        result.object.contractKey === "dashboards/dashboard" &&
        result.content.encoding === "json"
      ) {
        const { imageToken: _token, ...value } = result.content.value as Record<
          string,
          Wire.Json
        >;
        return value;
      }
      return result.content.value;
    };
    return Object.fromEntries(
      await Promise.all(
        Object.entries(value.sources).map(async ([name, source]) => [
          name,
          "objectId" in source
            ? await read(source.objectId)
            : await (async () => {
                const page = await this.client.request("objects.query", {
                  contractKey: source.contractKey,
                  limit: source.limit,
                });
                const values: Wire.Json[] = [];
                for (const item of page.items)
                  values.push(await read(item.objectId));
                return values;
              })(),
        ]),
      ),
    );
  }
}
