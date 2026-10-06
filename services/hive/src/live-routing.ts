import { IvyError, requireThat } from '../../../packages/contracts/src/errors.js';
import { SchemaValidators } from '../../../packages/contracts/src/schema.js';
import { managementFrameBytes } from '../../../packages/contracts/src/limits.js';
import type { ConnectionContext } from './kernel.js';
import type { LiveCatalog, ServiceNode, ToolBinding, Target } from './registry.js';
import type { Transport, Wire } from '../../../packages/contracts/src/generated.js';
import type { Schema } from '../../../packages/contracts/src/types.js';

export interface RoutingUpdate { node: ServiceNode; generation?: number; catalog?: LiveCatalog }
type Route = { node: ServiceNode; generation: number; namespaces: Set<string>; tools: Map<string, ToolBinding>; notifications: Map<string, Schema> };
export type PreparedCall = Target & { serviceNodeId: string; generation: number; definition: ToolBinding['definition']; definitionHash: string;
  arguments: Wire.Json; callerPrincipalId: string; operationId?: string };
type PrepareParams = Target & { qualifiedName: string; expectedDefinitionHash: string; arguments: Wire.Json; operationId?: string; expectedCallerPrincipalId?: string };

/** Live connection routing is memory state. Object operations retain their storage boundary. */
export class LiveRouting {
  private readonly validators: SchemaValidators | null;
  private readonly notificationValidators = new SchemaValidators();
  private readonly routes = new Map<string, Route>();
  constructor(private readonly validateMessages = false) { this.validators = validateMessages ? new SchemaValidators() : null; }
  clear(): void { this.routes.clear(); }
  retireUnconfiguredPrincipals(principalIds: readonly string[]): void {
    const configured = new Set(principalIds);
    for (const route of this.routes.values()) if (!configured.has(route.node.principalId)) {
      route.node.connected = false; route.node.synced = false; route.node.ready = false;
    }
  }
  update(value: RoutingUpdate): void {
    const prior = this.routes.get(value.node.serviceNodeId);
    if (!prior && value.generation === undefined) return;
    const generation = value.generation ?? prior!.generation;
    const route: Route = prior?.generation === generation ? prior : { node: value.node, generation, namespaces: new Set(), tools: new Map(), notifications: new Map() };
    route.node = structuredClone(value.node);
    if (value.catalog) {
      route.namespaces = new Set(value.catalog.namespaces); route.tools = new Map();
      for (const source of value.catalog.tools) {
        const binding = structuredClone(source); route.tools.set(binding.qualifiedName, binding);
      }
      route.notifications = new Map(value.catalog.notifications.map(notification => [notification.namespace + '\0' + notification.name + '\0' + notification.version, structuredClone(notification.payloadSchema)]));
    }
    this.routes.set(value.node.serviceNodeId, route);
  }
  disconnect(id: string, generation: number): boolean {
    const route = this.routes.get(id); if (route?.generation !== generation) return false;
    route.node.connected = false; route.node.synced = false; route.node.ready = false; return true;
  }
  suspend(id: string, generation: number): void {
    const route = this.routes.get(id); if (route?.generation === generation) route.node.synced = false;
  }
  caller(context: ConnectionContext): void {
    if (!context.serviceNodeId) return;
    const route = this.routes.get(context.serviceNodeId);
    requireThat(route?.node.connected && route.generation === context.generation, 'stale_generation', 'Service connection is no longer current.');
  }
  notification(context: ConnectionContext, params: Transport.ServiceNotification['params']): void {
    this.caller(context);
    const schema = context.serviceNodeId ? this.routes.get(context.serviceNodeId)?.notifications.get(params.namespace + '\0' + params.name + '\0' + params.version) : undefined;
    requireThat(schema, 'not_found', 'Transient notification name and version are not registered by this connection.');
    this.notificationValidators.validate(schema, params.payload, 1024 * 1024);
  }
  prepare(context: ConnectionContext, params: PrepareParams): PreparedCall {
    this.caller(context);
    requireThat(context.principalId && (params.expectedCallerPrincipalId === undefined || params.expectedCallerPrincipalId === context.principalId), 'caller_changed', 'Caller identity differs from the connected principal.');
    const namespace = params.qualifiedName.split('.')[0]!, explicit = params.serviceNodeId ?? params.resourceRef?.serviceNodeId;
    requireThat(!params.serviceNodeId || !params.resourceRef || params.serviceNodeId === params.resourceRef.serviceNodeId, 'target_conflict', 'Resource and node selectors disagree.');
    requireThat(!params.resourceRef || params.resourceRef.namespace === namespace, 'target_conflict', 'Resource and namespace selectors disagree.');
    const matches = (route: Route) => (!params.hostId || route.node.hostId === params.hostId) && (!params.serviceName || route.node.serviceName === params.serviceName);
    const eligible = (route: Route) => route.node.connected && route.node.synced && route.node.ready && route.node.desiredEnabled;
    let route: Route | undefined;
    if (explicit) {
      route = this.routes.get(explicit); requireThat(route, 'not_found', 'Service Node not found.');
      requireThat(matches(route), 'target_conflict', 'Selected resource has a different owner.');
      requireThat(route.node.connected, 'service_unavailable', 'Provider connection is closed.');
      requireThat(eligible(route), 'service_not_ready', 'Provider is not ready.');
      requireThat(route.namespaces.has(namespace), 'not_found', 'Namespace is absent from the selected provider.');
    } else {
      const candidates = [...this.routes.values()].filter(route => matches(route) && route.namespaces.has(namespace) && eligible(route));
      if (candidates.length > 1) throw new IvyError('ambiguous_service_node', 'Choose the owning Service Node.', 'not_executed', candidates.map(({node}) => ({serviceNodeId:node.serviceNodeId,hostId:node.hostId,serviceName:node.serviceName})));
      requireThat(candidates.length === 1, 'service_unavailable', 'No eligible provider matches the target.'); route = candidates[0]!;
    }
    const binding = route.tools.get(params.qualifiedName); requireThat(binding, 'not_found', 'Tool is absent from the selected provider.');
    requireThat(context.transport !== 'mcp' || binding.definition.discovery?.mcp, 'not_found', 'Tool is not published through MCP.');
    requireThat(binding.definitionHash === params.expectedDefinitionHash, 'tool_definition_changed', 'Tool definition changed.');
    if (this.validators) this.validators.validate(binding.definition.inputSchema, params.arguments, managementFrameBytes);
    return {serviceNodeId:route.node.serviceNodeId,generation:route.generation,definition:binding.definition,definitionHash:binding.definitionHash,arguments:params.arguments,callerPrincipalId:context.principalId,...(params.operationId===undefined?{}:{operationId:params.operationId})};
  }
  prepareDashboardImage(context: ConnectionContext, node: string, args: Wire.Json): PreparedCall {
    const binding = this.routes.get(node)?.tools.get("dashboards.image");
    requireThat(binding, "not_found", "Dashboard image endpoint is unavailable.");
    requireThat(binding.definition.annotations?.readOnlyHint === true,
      "provider_contract_error", "Dashboard image handler must be read-only.");
    return this.prepare(context, {
      serviceNodeId: node,
      serviceName: "dashboards",
      qualifiedName: "dashboards.image",
      expectedDefinitionHash: binding.definitionHash,
      arguments: args,
    });
  }
  complete(call: PreparedCall, result: unknown): unknown {
    const route = this.routes.get(call.serviceNodeId);
    if (!route?.node.connected || route.generation !== call.generation) throw new IvyError('stale_generation', 'Provider result belongs to an expired connection.', 'unknown');
    if (this.validators) {
      try { this.validators.validate(call.definition.outputSchema, result, managementFrameBytes); }
      catch { throw new IvyError('provider_contract_error', 'Provider returned output incompatible with the dispatched definition.', 'unknown'); }
    }
    return result;
  }
}
