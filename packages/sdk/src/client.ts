import {
  connectionInFlightRequests,
  managementFrameBytes,
} from "../../contracts/src/limits.js";
import { encodeJson } from "../../contracts/src/canonical-json.js";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import { RequestQueue } from "./request-queue.js";
import type {
  Operation,
  OperationName,
  Params,
  Result,
  Wire,
} from "../../contracts/src/generated.js";
import {
  deriveOperationId,
  operationId,
  parseOperationId,
} from "../../contracts/src/operation-id.js";
import { canonical } from "../../contracts/src/canonical-json.js";
import { nativeVersions } from "../../contracts/src/native-versions.js";
import {
  validateProviderNotificationFrame,
  validateChangeNotificationFrame,
} from "../../contracts/src/transport-validation.js";

export type {
  Agent,
  Chat,
  Secretary,
  TaskBoard,
  Automation,
  Operation,
  OperationName,
  Params,
  Result,
  Transport,
  Wire,
} from "../../contracts/src/generated.js";
export { IvyError, requireThat } from "../../contracts/src/errors.js";
export {
  canonical,
  deriveOperationId,
  operationId,
  parseOperationId,
  nativeVersions,
};
export {
  binaryObjectContentBytes,
  connectionInFlightRequests,
  consumerInFlightRequests,
  jsonObjectContentBytes,
  managementFrameBytes,
  mcpDiscoveryResultBytes,
  textObjectContentBytes,
} from "../../contracts/src/limits.js";
export interface RequestOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}
export interface ToolCallOptions extends RequestOptions {
  expectedCallerPrincipalId?: string;
}
export interface BoundTool extends Operation.ToolBinding {
  serviceNodeId: string;
  resourceRef?: Wire.ResourceRef;
}
export interface RpcClient {
  request<M extends OperationName>(
    method: M,
    params: Params<M>,
    options?: RequestOptions,
  ): Promise<Result<M>>;
}

/** A bundled query document must describe the same exact revision as its query row. */
export function queryDocument(item: Operation.QueryItem): Operation.ObjectRead {
  const read = item.document;
  requireThat(
    read &&
      read.object.id === item.objectId &&
      read.revision.objectId === item.objectId &&
      read.revision.revision === item.revision &&
      read.revision.contractVersion === item.contractVersion,
    "invalid_frame",
    "Hive query returned no matching document.",
  );
  return read;
}
const runtimeEpochs = new WeakMap<RpcClient, Promise<string>>();
export async function runtimeEpoch(client: RpcClient): Promise<string> {
  let pending = runtimeEpochs.get(client);
  if (!pending) {
    pending = client
      .request("system.status", {})
      .then((status) => status.runtimeEpoch);
    runtimeEpochs.set(client, pending);
    void pending.catch(() => {
      if (runtimeEpochs.get(client) === pending) runtimeEpochs.delete(client);
    });
  }
  return pending;
}
export async function newOperationId(
  client: RpcClient,
  nonce?: string,
): Promise<string> {
  return operationId(await runtimeEpoch(client), Date.now(), nonce);
}
const scopedOperationIds = new WeakMap<
  RpcClient,
  Map<string, { issuedAt: number; pending: Promise<string> }>
>();
/** Stable for one live workflow owner; durable workflows persist the returned value before effects. */
export function scopedOperationId(
  client: RpcClient,
  scope: unknown,
): Promise<string> {
  const key = canonical(scope, 65536);
  let values = scopedOperationIds.get(client);
  if (!values) {
    values = new Map();
    scopedOperationIds.set(client, values);
  }
  const now = Date.now();
  // Hive refuses an identity after 24 hours. Drop expired scopes in insertion order;
  // this makes cleanup amortized O(1) and keeps a long-lived connection bounded by recent work.
  for (const [scope, entry] of values) {
    if (entry.issuedAt + 24 * 60 * 60 * 1000 > now) break;
    values.delete(scope);
  }
  let entry = values.get(key);
  if (!entry) {
    const pending = newOperationId(client);
    entry = { issuedAt: now, pending };
    values.set(key, entry);
    void pending.catch(() => {
      if (values!.get(key)?.pending === pending) values!.delete(key);
    });
  }
  return entry.pending;
}
export type ProviderNotificationListener = (
  frame: import("../../contracts/src/generated.js").Transport.ProviderNotification,
) => void;
export type ProviderNotificationFilter =
  import("../../contracts/src/generated.js").Operation.NotificationFilter;
export interface InterfaceRequirement {
  namespace: string;
  interfaceVersion: string;
}
export type DiscoveryTarget = Omit<
  Params<"tools.list">,
  "namespace" | "namePrefix" | "cursor" | "limit"
> & { interfaceVersion?: string };

/** Discovery is explicit; a changed definition can never silently authorize another invocation. */
export async function discover(
  client: RpcClient,
  qualifiedName: string,
  target: DiscoveryTarget = {},
): Promise<BoundTool> {
  const separator = qualifiedName.indexOf(".");
  requireThat(
    separator > 0,
    "invalid_arguments",
    "Expected namespace and exact tool name.",
  );
  const namespace = qualifiedName.slice(0, separator),
    name = qualifiedName.slice(separator + 1);
  const { interfaceVersion, ...selectors } = target;
  let cursor: string | undefined;
  do {
    const page = await client.request("tools.list", {
      namespace,
      ...selectors,
      namePrefix: name,
      ...(cursor ? { cursor } : {}),
    });
    const binding = page.items.find(
      (tool) => tool.qualifiedName === qualifiedName,
    );
    if (binding) {
      requireThat(
        interfaceVersion === undefined ||
          binding.definition.interfaceVersion === interfaceVersion,
        "contract_version_conflict",
        "The selected provider exposes another namespace interface version.",
      );
      return {
        ...binding,
        serviceNodeId: page.provider.node.serviceNodeId,
        ...(selectors.resourceRef
          ? { resourceRef: selectors.resourceRef }
          : {}),
      };
    }
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  throw new IvyError(
    "not_found",
    "The exact tool is not in the selected provider catalog.",
  );
}

export function callBound(
  client: RpcClient,
  binding: BoundTool,
  args: Wire.Json,
  operationId?: string,
  options?: ToolCallOptions,
): Promise<Wire.Json> {
  return client.request(
    "tools.call",
    {
      qualifiedName: binding.qualifiedName,
      serviceNodeId: binding.serviceNodeId,
      expectedDefinitionHash: binding.definitionHash,
      arguments: args,
      ...(options?.expectedCallerPrincipalId === undefined
        ? {}
        : { expectedCallerPrincipalId: options.expectedCallerPrincipalId }),
      ...(binding.resourceRef ? { resourceRef: binding.resourceRef } : {}),
      ...(operationId === undefined ? {} : { operationId }),
    },
    options,
  );
}

const toolClients = new WeakMap<RpcClient, Map<string, BoundToolClient>>();
function freezeBinding(value: unknown): void {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) return;
  for (const child of Object.values(value)) freezeBinding(child);
  Object.freeze(value);
}

/** One selected provider. Mutations retain exact bindings; reads may refresh a rejected binding. */
export class BoundToolClient {
  private readonly bindings = new Map<string, Promise<BoundTool>>();
  private readonly interfaces: Map<string, string>;
  constructor(
    readonly client: RpcClient,
    readonly serviceNodeId: string,
    requirements: readonly InterfaceRequirement[],
    private readonly dynamicNamespace: string | null = null,
  ) {
    requireThat(
      serviceNodeId.length > 0,
      "invalid_arguments",
      "A bound Tool client requires one exact service node.",
    );
    requireThat(
      requirements.length > 0 || dynamicNamespace !== null,
      "invalid_arguments",
      "A bound Tool client requires an interface version or an explicit native namespace.",
    );
    for (const requirement of requirements)
      requireThat(
        /^[a-z][a-z0-9-]{0,63}$/.test(requirement.namespace) &&
          /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(
            requirement.interfaceVersion,
          ),
        "invalid_arguments",
        "A bound Tool interface requirement is invalid.",
      );
    this.interfaces = new Map(
      requirements.map((requirement) => [
        requirement.namespace,
        requirement.interfaceVersion,
      ]),
    );
    requireThat(
      this.interfaces.size === requirements.length,
      "invalid_arguments",
      "A bound Tool client cannot declare the same namespace twice.",
    );
  }
  async binding(
    qualifiedName: string,
    expectedDefinitionHash?: string,
  ): Promise<BoundTool> {
    let pending = this.bindings.get(qualifiedName);
    if (!pending) {
      requireThat(
        this.bindings.size < 256,
        "limit_exceeded",
        "The selected provider binding cache is full.",
      );
      const separator = qualifiedName.indexOf(".");
      requireThat(
        separator > 0,
        "invalid_arguments",
        "Expected namespace and exact tool name.",
      );
      const namespace = qualifiedName.slice(0, separator),
        interfaceVersion = this.interfaces.get(namespace);
      requireThat(
        interfaceVersion !== undefined || namespace === this.dynamicNamespace,
        "invalid_arguments",
        "The Tool namespace has no declared interface requirement.",
      );
      pending = discover(this.client, qualifiedName, {
        serviceNodeId: this.serviceNodeId,
        ...(interfaceVersion ? { interfaceVersion } : {}),
      }).then((binding) => {
        requireThat(
          binding.serviceNodeId === this.serviceNodeId,
          "target_conflict",
          "Discovery returned another selected provider.",
        );
        freezeBinding(binding);
        return binding;
      });
      this.bindings.set(qualifiedName, pending);
      // Failed discovery retained no contract. Only that failed lookup may be attempted again.
      void pending.catch(() => {
        if (this.bindings.get(qualifiedName) === pending)
          this.bindings.delete(qualifiedName);
      });
    }
    const binding = await pending;
    requireThat(
      expectedDefinitionHash === undefined ||
        binding.definitionHash === expectedDefinitionHash,
      "tool_definition_changed",
      "The selected Tool differs from the retained action definition.",
    );
    return binding;
  }
  /** Explicit rediscovery for a consumer that will reassess the replacement contract. */
  async refresh(binding: BoundTool): Promise<BoundTool> {
    requireThat(
      binding.serviceNodeId === this.serviceNodeId,
      "target_conflict",
      "Only the selected provider's binding can be refreshed.",
    );
    const pending = this.bindings.get(binding.qualifiedName);
    if (
      pending &&
      await pending === binding &&
      this.bindings.get(binding.qualifiedName) === pending
    )
      this.bindings.delete(binding.qualifiedName);
    return this.binding(binding.qualifiedName);
  }
  async call(
    qualifiedName: string,
    args: Wire.Json,
    operationId?: string,
    options?: ToolCallOptions,
  ): Promise<Wire.Json> {
    options?.signal?.throwIfAborted();
    const binding = await this.binding(qualifiedName);
    options?.signal?.throwIfAborted();
    return callBound(this.client, binding, args, operationId, options);
  }
  /** Refresh a definitively rejected read once, retaining provider, interface and arguments. */
  async read(qualifiedName: string, args: Wire.Json, options?: ToolCallOptions): Promise<Wire.Json> {
    options?.signal?.throwIfAborted();
    const original = structuredClone(args), binding = await this.binding(qualifiedName);
    requireThat(binding.definition.annotations?.readOnlyHint === true, "invalid_arguments", "A read requires a read-only Tool.");
    try {
      return await callBound(this.client, binding, original, undefined, options);
    } catch (error) {
      if (!(error instanceof IvyError) || error.code !== "tool_definition_changed" || error.outcome !== "not_executed") throw error;
      const fresh = await this.refresh(binding);
      if (fresh.definitionHash === binding.definitionHash || fresh.definition.annotations?.readOnlyHint !== true ||
          fresh.definition.interfaceVersion !== binding.definition.interfaceVersion) throw error;
      options?.signal?.throwIfAborted();
      return callBound(this.client, fresh, original, undefined, options);
    }
  }
}

function selectedTools(
  client: RpcClient,
  serviceNodeId: string,
  requirements: readonly InterfaceRequirement[],
  dynamicNamespace: string | null,
): BoundToolClient {
  let providers = toolClients.get(client);
  if (!providers) {
    providers = new Map();
    toolClients.set(client, providers);
  }
  const key =
    serviceNodeId +
    "\0" +
    canonical({
      requirements: [...requirements].sort((a, b) =>
        a.namespace.localeCompare(b.namespace),
      ),
      dynamicNamespace,
    });
  let selected = providers.get(key);
  if (!selected) {
    requireThat(
      providers.size < 64,
      "limit_exceeded",
      "The RPC client has too many selected Tool providers.",
    );
    selected = new BoundToolClient(
      client,
      serviceNodeId,
      requirements,
      dynamicNamespace,
    );
    providers.set(key, selected);
  }
  return selected;
}

/** Reused within one RPC client/connection. Stable service namespaces always name their exact interface version. */
export function serviceTools(
  client: RpcClient,
  serviceNodeId: string,
  requirements: readonly InterfaceRequirement[],
): BoundToolClient {
  requireThat(
    requirements.length > 0,
    "invalid_arguments",
    "Stable service Tools require at least one namespace interface version.",
  );
  return selectedTools(client, serviceNodeId, requirements, null);
}

/** Native Codex definitions are versioned and hash-fenced by the installed native catalog. */
export function nativeServiceTools(
  client: RpcClient,
  serviceNodeId: string,
): BoundToolClient {
  return selectedTools(client, serviceNodeId, [], "codex");
}

export function responseValue(frame: unknown, id: string): unknown {
  if (frame === null || typeof frame !== "object" || Array.isArray(frame))
    throw new IvyError(
      "invalid_frame",
      "Hive returned an invalid response.",
      "unknown",
    );
  const value = frame as Record<string, unknown>;
  if (value["jsonrpc"] !== "2.0" || "error" in value === "result" in value)
    throw new IvyError(
      "invalid_frame",
      "Hive response envelope is invalid.",
      "unknown",
    );
  const result = "result" in value;
  if (
    !Object.keys(value).every((key) =>
      (result
        ? ["jsonrpc", "id", "result"]
        : ["jsonrpc", "id", "error"]
      ).includes(key),
    )
  )
    throw new IvyError(
      "invalid_frame",
      "Hive response envelope contains unexpected fields.",
      "unknown",
    );
  if ("result" in value) {
    if (value["id"] !== id)
      throw new IvyError(
        "invalid_frame",
        "Hive response identity is invalid.",
        "unknown",
      );
    return value["result"];
  }
  const error = value["error"] as Partial<Wire.Error> | null;
  if (
    !error ||
    !Object.keys(error).every((key) =>
      ["code", "message", "data"].includes(key),
    ) ||
    !Number.isSafeInteger(error.code) ||
    typeof error.message !== "string" ||
    error.message.length < 1 ||
    error.message.length > 2048 ||
    !error.data ||
    !Object.keys(error.data).every((key) =>
      ["code", "outcome", "details"].includes(key),
    ) ||
    typeof error.data.code !== "string" ||
    error.data.code.length < 1 ||
    error.data.code.length > 128 ||
    !["not_executed", "completed", "unknown"].includes(error.data.outcome)
  )
    throw new IvyError(
      "invalid_frame",
      "Hive returned an invalid error.",
      "unknown",
    );
  // Authentication/frame limits can refuse the HTTP request before reading its correlation ID.
  if (
    value["id"] !== id &&
    !(value["id"] === null && error.data.outcome === "not_executed")
  )
    throw new IvyError(
      "invalid_frame",
      "Hive error identity is invalid.",
      "unknown",
    );
  throw new IvyError(
    error.data.code,
    error.message,
    error.data.outcome,
    error.data.details,
  );
}

export function baseUrl(value: string): URL {
  const url = new URL(value);
  requireThat(
    ["https:", "http:"].includes(url.protocol) &&
      !url.username &&
      !url.password &&
      !url.search &&
      !url.hash,
    "invalid_arguments",
    "Expected the canonical Hive base URL.",
  );
  requireThat(
    url.protocol === "https:" ||
      ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname),
    "invalid_arguments",
    "Remote Hive connections require HTTPS.",
  );
  url.pathname = url.pathname.replace(/\/$/, "") + "/";
  return url;
}

/** Admission never retries sent work; uncertain mutations keep their original identity. */
export class HiveClient implements RpcClient {
  readonly base: URL;
  private active = 0;
  private readonly queue: RequestQueue | null;
  constructor(
    value: string,
    private readonly options: {
      credential?: string;
      fetch?: typeof fetch;
      /** Queue calls routed to providers beyond this many; Hive holds each one until the provider answers. */
      providerCalls?: number;
      /** Bound all sent requests, while allowing ordinary reads past waiting provider calls. */
      requestCalls?: number;
      /** Maximum requests waiting to be sent. */
      queuedRequests?: number;
    } = {},
  ) {
    this.base = baseUrl(value);
    const maximum = options.requestCalls ?? connectionInFlightRequests;
    requireThat(
      Number.isSafeInteger(maximum) &&
        maximum > 0 &&
        maximum <= connectionInFlightRequests,
      "invalid_arguments",
      "Invalid client concurrency limit.",
    );
    this.queue =
      options.requestCalls || options.providerCalls
        ? new RequestQueue(
            maximum,
            options.providerCalls ?? maximum,
            options.queuedRequests ?? 128,
          )
        : null;
  }
  async request<M extends OperationName>(
    method: M,
    params: Params<M>,
    options: RequestOptions = {},
  ): Promise<Result<M>> {
    const controller = new AbortController();
    const timer = setTimeout(
      () => controller.abort(),
      options.timeoutMs ?? 35_000,
    );
    const signal = options.signal
      ? AbortSignal.any([controller.signal, options.signal])
      : controller.signal;
    let release: (() => void) | undefined;
    let sent = false;
    try {
      signal.throwIfAborted();
      if (this.queue)
        release = await this.queue.acquire(
          method === "tools.call" || method === "discovery.call",
          signal,
        );
      signal.throwIfAborted();
      sent = true;
      return await this.send(method, params, { ...options, signal });
    } catch (error) {
      if (!sent && controller.signal.aborted && !options.signal?.aborted)
        throw new IvyError(
          "deadline_exceeded",
          "Hive request expired before sending.",
          "not_executed",
        );
      throw error;
    } finally {
      release?.();
      clearTimeout(timer);
    }
  }
  private async send<M extends OperationName>(
    method: M,
    params: Params<M>,
    options: RequestOptions & { signal: AbortSignal },
  ): Promise<Result<M>> {
    requireThat(
      this.active < connectionInFlightRequests,
      "limit_exceeded",
      "Hive client request limit reached.",
    );
    options.signal?.throwIfAborted();
    const id = crypto.randomUUID(),
      body = encodeJson({ jsonrpc: "2.0", id, method, params });
    const signal = options.signal;
    this.active++;
    try {
      const response = await (this.options.fetch ?? fetch)(
        new URL("api/v1/rpc", this.base),
        {
          method: "POST",
          credentials: "same-origin",
          redirect: "error",
          signal,
          headers: {
            "Content-Type": "application/json",
            ...(this.options.credential
              ? { Authorization: "Bearer " + this.options.credential }
              : {}),
          },
          body,
        },
      );
      const reader = response.body?.getReader();
      if (!reader)
        throw new IvyError(
          "invalid_frame",
          "Hive returned no response body.",
          "unknown",
        );
      const chunks: Uint8Array[] = [];
      let size = 0;
      try {
        for (;;) {
          const part = await reader.read();
          if (part.done) break;
          size += part.value.byteLength;
          if (size > managementFrameBytes) {
            await reader.cancel();
            throw new IvyError(
              "result_too_large",
              "Hive response exceeds its frame limit.",
              "unknown",
            );
          }
          chunks.push(part.value);
        }
      } finally {
        reader.releaseLock();
      }
      const buffer = new Uint8Array(size);
      let offset = 0;
      for (const chunk of chunks) {
        buffer.set(chunk, offset);
        offset += chunk.byteLength;
      }
      let frame: unknown;
      try {
        frame = JSON.parse(
          new TextDecoder("utf-8", { fatal: true }).decode(buffer),
        );
      } catch {
        throw new IvyError(
          "invalid_frame",
          "Hive returned malformed JSON.",
          "unknown",
        );
      }
      return responseValue(frame, id) as Result<M>;
    } catch (error) {
      if (error instanceof IvyError) throw error;
      throw new IvyError(
        "outcome_unknown",
        "The request connection failed or timed out. Reconcile its original operation identity.",
        "unknown",
      );
    } finally {
      this.active--;
    }
  }
}

/** Browser code receives no API credential and stores no authentication material. */
export function browserClient(value: string): HiveClient {
  const base = baseUrl(value);
  requireThat(
    base.origin === location.origin,
    "invalid_arguments",
    "Browser Hive access must stay on the current origin.",
  );
  // Pages share one per-client Hive request budget; a slow provider must not consume all of it.
  return new HiveClient(base.href, {
    requestCalls: 16,
    providerCalls: 8,
    queuedRequests: 128,
  });
}

/** Browser-only provider notifications. Commands keep using the reconciled HTTP client. */
export class BrowserNotifications {
  readonly base: URL;
  private readonly listeners = new Map<
    ProviderNotificationListener,
    ProviderNotificationFilter
  >();
  private socket: WebSocket | null = null;
  private readonly changeListeners = new Map<
    (scopes: string[]) => void,
    readonly string[]
  >();
  private readonly statusListeners = new Set<(ready: boolean) => void>();
  private ready = false;
  private deadline: ReturnType<typeof setTimeout> | undefined;
  private heartbeat: ReturnType<typeof setTimeout> | undefined;
  private reconnect: ReturnType<typeof setTimeout> | undefined;
  private reconnectMs = 500;
  private pending: {
    id: string;
    signature: string;
    count: number;
    changes: number;
  } | null = null;
  private syncedSignature: string | null = null;
  constructor(value: string) {
    this.base = baseUrl(value);
    requireThat(
      this.base.origin === location.origin,
      "invalid_arguments",
      "Browser Hive notifications must stay on the current origin.",
    );
  }
  subscribe(
    filter: ProviderNotificationFilter,
    listener: ProviderNotificationListener,
  ): () => void {
    this.listeners.set(listener, structuredClone(filter));
    this.changed();
    return () => {
      this.listeners.delete(listener);
      this.changed();
    };
  }
  get connected(): boolean {
    return this.ready;
  }
  onStatus(listener: (ready: boolean) => void): () => void {
    this.statusListeners.add(listener);
    listener(this.ready);
    return () => {
      this.statusListeners.delete(listener);
    };
  }
  subscribeChanges(
    scopes: readonly string[],
    listener: (scopes: string[]) => void,
  ): () => void {
    this.changeListeners.set(listener, [...scopes]);
    this.changed();
    return () => {
      this.changeListeners.delete(listener);
      this.changed();
    };
  }
  /** Resume after browser suspension or a network change with a fresh subscription. */
  reconnectNow(): void {
    if (this.socket) this.disconnected(this.socket);
    clearTimeout(this.reconnect);
    this.connect();
  }
  private setReady(value: boolean): void {
    if (this.ready === value) return;
    this.ready = value;
    for (const listener of this.statusListeners) {
      try {
        listener(value);
      } catch {
        /* Subscribers are isolated. */
      }
    }
  }
  private wanted(): boolean {
    return this.listeners.size + this.changeListeners.size > 0;
  }
  private changed(): void {
    if (!this.changeListeners.size) clearTimeout(this.heartbeat);
    if (this.wanted()) {
      this.connect();
      this.sync();
      return;
    }
    if (this.socket) this.disconnected(this.socket);
    clearTimeout(this.reconnect);
    this.reconnect = undefined;
  }
  private disconnected(socket: WebSocket): void {
    if (this.socket !== socket) return;
    this.socket = null;
    this.pending = null;
    this.syncedSignature = null;
    clearTimeout(this.deadline);
    clearTimeout(this.heartbeat);
    this.setReady(false);
    socket.close();
    this.retry();
  }
  private retry(): void {
    if (!this.wanted()) return;
    clearTimeout(this.reconnect);
    const wait = this.reconnectMs;
    this.reconnectMs = Math.min(10_000, this.reconnectMs * 2);
    this.reconnect = setTimeout(() => this.connect(), wait);
  }
  private filters(): ProviderNotificationFilter[] {
    const unique = new Map<string, ProviderNotificationFilter>();
    for (const filter of this.listeners.values())
      unique.set(
        `${filter.namespace}\u0000${filter.name}\u0000${filter.version}\u0000${filter.serviceNodeId ?? ""}`,
        filter,
      );
    return [...unique.values()];
  }
  private sync(): void {
    const socket = this.socket;
    if (
      !this.wanted() ||
      !socket ||
      socket.readyState !== WebSocket.OPEN ||
      this.pending
    )
      return;
    const filters = this.filters(),
      changes = [...new Set([...this.changeListeners.values()].flat())].sort(),
      signature = canonical({ filters, changes });
    if (signature === this.syncedSignature) return;
    const id = crypto.randomUUID();
    this.setReady(false);
    this.pending = {
      id,
      signature,
      count: filters.length,
      changes: changes.length,
    };
    clearTimeout(this.deadline);
    this.deadline = setTimeout(() => this.disconnected(socket), 5_000);
    try {
      socket.send(
        encodeJson({
          jsonrpc: "2.0",
          id,
          method: "notifications.subscribe",
          params: { filters, ...(changes.length ? { changes } : {}) },
        }),
      );
    } catch {
      this.disconnected(socket);
    }
  }
  private connect(): void {
    if (
      !this.wanted() ||
      (this.socket &&
        (this.socket.readyState === WebSocket.CONNECTING ||
          this.socket.readyState === WebSocket.OPEN))
    )
      return;
    clearTimeout(this.reconnect);
    this.reconnect = undefined;
    const url = new URL("ws", this.base);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    let socket: WebSocket;
    try {
      socket = new WebSocket(url);
    } catch {
      this.retry();
      return;
    }
    this.socket = socket;
    this.deadline = setTimeout(() => this.disconnected(socket), 10_000);
    socket.addEventListener("open", () => {
      if (this.socket === socket) {
        this.sync();
      }
    });
    socket.addEventListener("message", (event) => {
      if (this.socket !== socket || typeof event.data !== "string") return;
      if (this.changeListeners.size) {
        clearTimeout(this.heartbeat);
        this.heartbeat = setTimeout(() => this.disconnected(socket), 35_000);
      }
      let frame: unknown;
      try {
        frame = JSON.parse(event.data);
      } catch {
        this.disconnected(socket);
        return;
      }
      if (
        frame &&
        typeof frame === "object" &&
        !Array.isArray(frame) &&
        "id" in frame
      ) {
        const pending = this.pending;
        if (
          !pending ||
          String((frame as Record<string, unknown>)["id"]) !== pending.id
        ) {
          this.disconnected(socket);
          return;
        }
        try {
          const result = responseValue(frame, pending.id);
          requireThat(
            result !== null &&
              typeof result === "object" &&
              !Array.isArray(result) &&
              Object.keys(result).length === (pending.changes ? 2 : 1) &&
              (!pending.changes ||
                (result as Record<string, unknown>)["changes"] ===
                  pending.changes) &&
              (result as Record<string, unknown>)["subscribed"] ===
                pending.count,
            "invalid_frame",
            "Hive returned an invalid notification subscription result.",
          );
          this.syncedSignature = pending.signature;
          this.pending = null;
          clearTimeout(this.deadline);
          this.reconnectMs = 500;
          this.sync();
          if (this.socket === socket && !this.pending) this.setReady(true);
        } catch {
          this.disconnected(socket);
        }
        return;
      }
      if (
        frame &&
        typeof frame === "object" &&
        "method" in frame &&
        frame.method === "notifications.changed"
      ) {
        try {
          validateChangeNotificationFrame(frame);
        } catch {
          this.disconnected(socket);
          return;
        }
        const scopes = frame.params.scopes;
        for (const [listener, watched] of this.changeListeners) {
          const selected = scopes.filter((scope) =>
            watched.some(
              (value) =>
                scope === value ||
                scope.startsWith(value + "/") ||
                value.startsWith(scope + "/"),
            ),
          );
          if (selected.length) {
            try {
              listener(selected);
            } catch {
              /* Subscribers are isolated. */
            }
          }
        }
        return;
      }
      try {
        validateProviderNotificationFrame(frame);
      } catch {
        this.disconnected(socket);
        return;
      }
      for (const [listener, filter] of this.listeners) {
        if (
          filter.namespace !== frame.params.namespace ||
          filter.name !== frame.params.name ||
          filter.version !== frame.params.version ||
          (filter.serviceNodeId !== undefined &&
            filter.serviceNodeId !== frame.params.serviceNodeId)
        )
          continue;
        try {
          listener(frame);
        } catch {
          /* One view cannot break other subscribers. */
        }
      }
    });
    socket.addEventListener("close", () => {
      this.disconnected(socket);
    });
    socket.addEventListener("error", () => {
      this.disconnected(socket);
    });
  }
}

export function browserNotifications(value: string): BrowserNotifications {
  return new BrowserNotifications(value);
}
