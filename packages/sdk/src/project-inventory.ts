import type { Agent } from "../../contracts/src/generated.js";
import type { RpcClient, RequestOptions } from "./client.js";
import { IvyError, serviceTools } from './client.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { backendArguments, sameNativeBackend } from '../../contracts/src/native-backend.js';

/** Scoped names require a current account observation; unscoped retained inventory also covers offline hosts. */
export async function hostProject(
  client: RpcClient,
  hostId: string | undefined,
  projectId: string,
  options?: RequestOptions,
  selection?: { backend?: Agent.BackendIdentity; serviceNodeId?: string; path?: string },
): Promise<Agent.ProjectSummary | null> {
  let cursor: string | undefined;
  const cursors = new Set<string>();
  let found: Agent.ProjectSummary | null = null, owner: string | undefined, observed = 0;
  do {
    const page = await client.request(
      "inventory.list",
      {
        serviceName: "agent-manager",
        namespace: selection?.backend?.backendId ?? "codex",
        kind: "project",
        ...(selection?.serviceNodeId ? { serviceNodeId: selection.serviceNodeId } : {}),
        ...(hostId ? { hostId } : {}),
        limit: 100,
        ...(cursor ? { cursor } : {}),
      },
      options,
    );
    for (const item of page.items) {
      if (item.resourceRef.nativeId !== projectId) continue;
      if (selection && (item.resourceRef.namespace !== (selection.backend?.backendId ?? 'codex') ||
        selection.serviceNodeId && item.resourceRef.serviceNodeId !== selection.serviceNodeId)) continue;
      const value = item.summary;
      if (
        value &&
        typeof value === "object" &&
        !Array.isArray(value) &&
        value["nativeId"] === projectId &&
        typeof value["name"] === "string" &&
        Array.isArray(value["paths"])
      ) {
        if (selection?.path && !value['paths'].includes(selection.path)) continue;
        if (!selection) return value as Agent.ProjectSummary;
        if (found) return null;
        found = value as Agent.ProjectSummary;
        owner = item.resourceRef.serviceNodeId;
      }
    }
    cursor = page.nextCursor ?? undefined;
    observed += page.items.length;
    if (observed > 1000 || cursor && cursors.has(cursor) || cursors.size >= 20)
      throw new IvyError('native_projects_unavailable', 'Project discovery exceeded its bounded snapshot.');
    if (cursor) cursors.add(cursor);
  } while (cursor);
  if (!found || !owner || !selection?.backend) return found;
  // Namespace reuse cannot prove an account. Inventory selects the address; the
  // current Agent envelope must confirm the complete owner before displaying it.
  const current = await serviceTools(client, owner, [{ namespace: 'agent', interfaceVersion: '1.0.0' }])
    .read('agent.projects', backendArguments({}, selection.backend), options);
  validateAgent('ProjectsResult', current);
  const projects = current as Agent.ProjectsResult;
  if (!sameNativeBackend(projects.backend, selection.backend))
    throw new IvyError('native_owner_mismatch', 'Projects must retain the selected provider and account.');
  const matches = projects.projects.filter(project => project.nativeId === projectId && (!selection.path || project.paths.includes(selection.path)));
  return matches.length === 1 ? matches[0]! : null;
}
