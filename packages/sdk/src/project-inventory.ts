import type { Agent } from "../../contracts/src/generated.js";
import type { RpcClient, RequestOptions } from "./client.js";

/** Project identities belong to their host; retained inventory also covers offline hosts. */
export async function hostProject(
  client: RpcClient,
  hostId: string | undefined,
  projectId: string,
  options?: RequestOptions,
): Promise<Agent.ProjectSummary | null> {
  let cursor: string | undefined;
  do {
    const page = await client.request(
      "inventory.list",
      {
        serviceName: "agent-manager",
        namespace: "codex",
        kind: "project",
        ...(hostId ? { hostId } : {}),
        limit: 100,
        ...(cursor ? { cursor } : {}),
      },
      options,
    );
    for (const item of page.items) {
      if (item.resourceRef.nativeId !== projectId) continue;
      const value = item.summary;
      if (
        value &&
        typeof value === "object" &&
        !Array.isArray(value) &&
        value["nativeId"] === projectId &&
        typeof value["name"] === "string" &&
        Array.isArray(value["paths"])
      )
        return value as Agent.ProjectSummary;
    }
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  return null;
}
