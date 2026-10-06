import { discover, serviceTools } from "../../../packages/sdk/src/client.js";
import type { Agent } from "../../../packages/sdk/src/client.js";
import {
  modelsFrom,
  nativeModels,
  type NativeModel,
  record,
  schemaNode,
} from "../../../packages/ui-client/src/native";
import { client } from "./runtime";

export async function executionEnvironments(signal: AbortSignal) {
  const nodes = [];
  let cursor: string | undefined;
  do {
    const page = await client.request(
      "serviceNodes.list",
      {
        serviceName: "agent-manager",
        limit: 200,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    );
    nodes.push(...page.items);
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  const observations = await Promise.all(
    nodes
      .filter((node) => node.connected && node.synced && node.ready)
      .map(async (node) => {
        try {
          const tools = serviceTools(client, node.serviceNodeId, [
            { namespace: "agent", interfaceVersion: "1.0.0" },
          ]);
          return {
            node,
            profile: (await tools.call(
              "agent.capabilities",
              {},
            )) as Agent.CapabilityProfile,
            projects: (await tools.call(
              "agent.projects",
              {},
            )) as Agent.ProjectsResult,
            models: modelsFrom(
              await nativeModels(client, node.serviceNodeId, signal),
            ),
            bindings: await Promise.all(
              ["thread/start", "turn/start"].map((method) =>
                discover(client, "codex." + method, {
                  serviceNodeId: node.serviceNodeId,
                }),
              ),
            ),
          };
        } catch {
          return null;
        }
      }),
  );
  const capabilities = new Map<string, string>();
  const projects = new Map<
    string,
    { id: string; name: string; path: string; hosts: Set<string> }
  >();
  for (const observation of observations)
    if (observation) {
      for (const value of observation.profile.capabilities)
        capabilities.set(value.key, value.label);
      for (const project of observation.projects.projects)
        for (const path of project.paths) {
          const key = JSON.stringify([project.nativeId, path]);
          const saved = projects.get(key) ?? {
            id: project.nativeId,
            name: project.name,
            path,
            hosts: new Set<string>(),
          };
          saved.hosts.add(observation.node.hostId);
          projects.set(key, saved);
        }
    }
  return {
    nodes,
    hosts: [...new Set(nodes.map((node) => node.hostId))].sort(),
    capabilities: [...capabilities]
      .map(([key, label]) => ({ key, label }))
      .sort((a, b) => a.key.localeCompare(b.key)),
    projects: [...projects]
      .map(([key, value]) => ({
        key,
        id: value.id,
        name: value.name,
        path: value.path,
        hosts: [...value.hosts].sort(),
      }))
      .sort(
        (a, b) => a.name.localeCompare(b.name) || a.path.localeCompare(b.path),
      ),
    agents: observations.filter((value) => value !== null),
  };
}
export type ExecutionAgent = Awaited<
  ReturnType<typeof executionEnvironments>
>["agents"][number];
export function executionModels(agents: ExecutionAgent[]) {
  // A host can run several providers. Any of its agents may serve a model;
  // defaults for multiple hosts still require support on each eligible host.
  const hosts = new Map<string, Map<string, NativeModel>>();
  for (const agent of agents) {
    const models =
      hosts.get(agent.node.hostId) ?? new Map<string, NativeModel>();
    hosts.set(agent.node.hostId, models);
    for (const model of agent.models) {
      const previous = models.get(model.model);
      models.set(
        model.model,
        previous
          ? {
              ...previous,
              hidden: previous.hidden && model.hidden,
              efforts: [...new Set([...previous.efforts, ...model.efforts])],
            }
          : model,
      );
    }
  }
  const catalogs = [...hosts.values()];
  return [...(catalogs[0]?.values() ?? [])]
    .filter((model) => catalogs.every((catalog) => catalog.has(model.model)))
    .map((model) => ({
      ...model,
      efforts: model.efforts.filter((effort) =>
        catalogs.every((catalog) =>
          catalog.get(model.model)!.efforts.includes(effort),
        ),
      ),
    }));
}
/** Every model any agent offers, with the hosts that lack it, so partial support is visible. */
export function modelAvailability(agents: ExecutionAgent[]) {
  const models = new Map<
    string,
    { model: string; name: string; hidden: boolean; hosts: Set<string> }
  >();
  for (const agent of agents)
    for (const model of agent.models) {
      const entry = models.get(model.model) ?? {
        model: model.model,
        name: model.name,
        hidden: model.hidden,
        hosts: new Set<string>(),
      };
      entry.hosts.add(agent.node.hostId);
      models.set(model.model, entry);
    }
  const hosts = [...new Set(agents.map((agent) => agent.node.hostId))];
  return [...models.values()].map((entry) => ({
    model: entry.model,
    name: entry.name,
    hidden: entry.hidden,
    missing: hosts.filter((host) => !entry.hosts.has(host)).sort(),
  }));
}
export const executionSpeedAvailable = (
  agents: ExecutionAgent[],
  model?: string,
  effort?: string,
) => {
  const supportedHosts = new Set(
    agents
      .filter(
        (agent) =>
          (!model ||
            agent.models.some(
              (value) =>
                value.model === model &&
                (!effort || value.efforts.includes(effort)),
            )) &&
          agent.bindings.every(
            (binding) =>
              "serviceTier" in
              record(
                schemaNode(
                  binding.definition.inputSchema,
                  binding.definition.inputSchema,
                ).properties,
              ),
          ),
      )
      .map((agent) => agent.node.hostId),
  );
  return (
    agents.length > 0 &&
    agents.every((agent) => supportedHosts.has(agent.node.hostId))
  );
};
