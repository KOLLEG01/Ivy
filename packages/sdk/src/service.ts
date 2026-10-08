import {
  connectionInFlightRequests,
  managementFrameBytes,
  managementSocketBufferBytes,
} from "../../contracts/src/limits.js";
import { WebSocket } from "ws";
import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import { toolDefinitionHash } from "../../contracts/src/tool-definition.js";
import { encodeJson } from "../../contracts/src/canonical-json.js";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import { validateShared } from "../../contracts/src/wire-validation.js";
import {
  validateEventAvailabilityFrame,
  validateProviderCallFrame,
  validateProviderNotificationFrame,
} from "../../contracts/src/transport-validation.js";
import type {
  Operation,
  OperationName,
  Params,
  Result,
  Transport,
  Wire,
} from "../../contracts/src/generated.js";
import { baseUrl, responseValue } from "./client.js";
import type { RequestOptions, RpcClient } from "./client.js";

export interface InvocationContext {
  callerPrincipalId: string;
  operationId?: string;
  generation: number;
  signal: AbortSignal;
}
export type ToolHandler = (
  args: Record<string, Wire.Json>,
  context: InvocationContext,
) => Promise<Wire.Json> | Wire.Json;
export type JsonToolHandler = (
  args: Wire.Json,
  context: InvocationContext,
) => Promise<Wire.Json> | Wire.Json;
export interface ServiceOptions {
  publicBaseUrl: string;
  credential: () => string | Promise<string>;
  identity: Wire.ServiceConnect;
  registry: () => Wire.RegistrySync | Promise<Wire.RegistrySync>;
  handlers: Record<string, ToolHandler>;
  jsonHandlers?: Record<string, JsonToolHandler>;
  reconcile: (connection: ServiceConnection) => Promise<void>;
  readiness?: (
    connection: ServiceConnection,
  ) => Promise<{ ready: boolean; diagnostics: Wire.Diagnostic[] }>;
  onState?: (state: {
    status:
      "connecting" | "syncing" | "ready" | "degraded" | "offline" | "stopped";
    generation?: number;
    code?: string;
    message?: string;
    phase?: string;
  }) => void;
  notificationFilters?: () =>
    Operation.NotificationFilter[] | Promise<Operation.NotificationFilter[]>;
  onNotification?: (value: Transport.ProviderNotification) => void;
  reconnectMinMs?: number;
  reconnectMaxMs?: number;
  heartbeatMs?: number;
}
interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
  cleanup: () => void;
}
interface EventWaiter {
  afterSequence: number;
  finish: (error?: unknown) => void;
}
async function dependency<T>(
  value: T | Promise<T>,
  signal: AbortSignal,
): Promise<T> {
  signal.throwIfAborted();
  return new Promise<T>((resolve, reject) => {
    const abort = () => {
      cleanup();
      reject(
        new IvyError(
          "service_unavailable",
          "Dependency observation was cancelled; accepted work must be reconciled.",
          "unknown",
        ),
      );
    };
    const timer = setTimeout(() => {
      cleanup();
      reject(
        new IvyError(
          "deadline_exceeded",
          "Service dependency did not finish before its deadline.",
          "unknown",
        ),
      );
    }, 30_000);
    const cleanup = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", abort);
    };
    signal.addEventListener("abort", abort, { once: true });
    Promise.resolve(value).then(
      (result) => {
        cleanup();
        resolve(result);
      },
      (error) => {
        cleanup();
        reject(error);
      },
    );
  });
}

/** One socket generation. Closing it never queues requests for its replacement. */
export class ServiceConnection implements RpcClient {
  readonly socket: WebSocket;
  readonly controller = new AbortController();
  readonly closed: Promise<void>;
  readonly opened: Promise<void>;
  generation = 0;
  get serviceNodeId(): string {
    return this.owner.options.identity.serviceNodeId;
  }
  ready = false;
  private readonly pending = new Map<string, Pending>();
  private readonly eventWaiters = new Set<EventWaiter>();
  private eventsAvailableThroughSequence = 0;
  private bindings = new Map<
    string,
    { hash: string; handler: JsonToolHandler }
  >();
  private finish!: () => void;
  private openReject!: (error: Error) => void;
  private ended = false;
  constructor(
    url: URL,
    credential: string,
    private readonly owner: ServiceClient,
  ) {
    this.closed = new Promise((resolve) => {
      this.finish = resolve;
    });
    // Hive binds browser-like WebSocket handshakes to its exact public origin.
    // Native services still use bearer authentication, but must provide the
    // same explicit origin because ws does not synthesize one like a browser.
    const origin = new URL(owner.options.publicBaseUrl).origin;
    this.socket = new WebSocket(url, {
      headers: { Authorization: "Bearer " + credential, Origin: origin },
      handshakeTimeout: 10_000,
      maxPayload: managementFrameBytes,
      perMessageDeflate: false,
    });
    this.opened = new Promise((resolve, reject) => {
      this.openReject = reject;
      this.socket.once("open", () => resolve());
    });
    this.socket.on("error", () =>
      this.close(
        new IvyError(
          "service_unavailable",
          "Hive WebSocket failed.",
          "unknown",
        ),
      ),
    );
    this.socket.on("close", () =>
      this.close(
        new IvyError(
          "outcome_unknown",
          "Hive connection closed; reconcile accepted operations.",
          "unknown",
        ),
      ),
    );
    this.socket.on("message", (bytes, binary) => {
      if (binary) {
        this.close(
          new IvyError(
            "invalid_frame",
            "Hive sent a binary protocol frame.",
            "unknown",
          ),
        );
        return;
      }
      try {
        const frame = JSON.parse(
          new TextDecoder("utf-8", { fatal: true }).decode(
            Buffer.isBuffer(bytes) ? bytes : Buffer.concat(bytes as Buffer[]),
          ),
        ) as Record<string, unknown>;
        if (!frame || typeof frame !== "object" || Array.isArray(frame))
          throw new Error("Invalid frame");
        if (frame["method"] === "provider.invoke") {
          void this.invoke(frame);
          return;
        }
        if (frame["method"] === "events.available" && !("id" in frame)) {
          validateEventAvailabilityFrame(frame);
          this.eventsAvailable(frame);
          return;
        }
        if (frame["method"] === "notifications.provider" && !("id" in frame)) {
          validateProviderNotificationFrame(frame);
          this.owner.notify(frame);
          return;
        }
        const id = String(frame["id"]),
          pending = this.pending.get(id);
        if (!pending) return; // Timed-out request IDs are never reused.
        this.pending.delete(id);
        clearTimeout(pending.timer);
        pending.cleanup();
        try {
          pending.resolve(responseValue(frame, id));
        } catch (error) {
          pending.reject(
            error instanceof IvyError
              ? error
              : new IvyError(
                  "invalid_frame",
                  "Invalid Hive result.",
                  "unknown",
                ),
          );
        }
      } catch {
        this.close(
          new IvyError(
            "invalid_frame",
            "Hive sent malformed protocol data.",
            "unknown",
          ),
        );
      }
    });
  }
  get signal(): AbortSignal {
    return this.controller.signal;
  }
  private registryBindings(
    registry: Wire.RegistrySync,
  ): Map<string, { hash: string; handler: JsonToolHandler }> {
    const bindings = new Map<
      string,
      { hash: string; handler: JsonToolHandler }
    >();
    for (const namespace of registry.namespaces)
      for (const definition of namespace.tools) {
        const name = definition.namespace + "." + definition.name;
        const objectHandler = Object.hasOwn(this.owner.options.handlers, name),
          jsonHandler = Object.hasOwn(
            this.owner.options.jsonHandlers ?? {},
            name,
          );
        requireThat(
          !bindings.has(name) &&
            objectHandler !== jsonHandler &&
            typeof (objectHandler
              ? this.owner.options.handlers[name]
              : this.owner.options.jsonHandlers?.[name]) === "function",
          "registry_invalid",
          "Every declared tool needs exactly one concrete object or JSON handler.",
        );
        const handler = jsonHandler
          ? this.owner.options.jsonHandlers![name]!
          : this.owner.options.handlers[name]!;
        bindings.set(name, {
          hash: toolDefinitionHash(definition),
          handler: handler as JsonToolHandler,
        });
      }
    return bindings;
  }
  private registrySnapshot(registry: Wire.RegistrySync): Wire.RegistrySync {
    validateShared("RegistrySync", registry);
    const snapshot = structuredClone(registry);
    const freeze = (value: unknown): void => {
      if (value && typeof value === "object") {
        for (const child of Object.values(value)) freeze(child);
        Object.freeze(value);
      }
    };
    freeze(snapshot);
    return snapshot;
  }
  setRegistry(registry: Wire.RegistrySync): void {
    this.bindings = this.registryBindings(this.registrySnapshot(registry));
  }
  async prepareRegistry(
    registry: Wire.RegistrySync,
  ): Promise<Wire.RegistrySync> {
    this.signal.throwIfAborted();
    const snapshot = this.registrySnapshot(registry),
      bindings = this.registryBindings(snapshot);
    // Hive admits schemas once at registry.sync; the SDK binds concrete handlers only.
    this.bindings = bindings;
    return snapshot;
  }
  private send(value: unknown): void {
    requireThat(
      !this.ended && this.socket.readyState === WebSocket.OPEN,
      "service_unavailable",
      "Hive connection is unavailable.",
    );
    const encoded = encodeJson(value);
    requireThat(
      this.socket.bufferedAmount + Buffer.byteLength(encoded) <=
        managementSocketBufferBytes,
      "limit_exceeded",
      "Service socket buffer is full.",
    );
    this.socket.send(encoded, (error) => {
      if (error)
        this.close(
          new IvyError("outcome_unknown", "Hive write failed.", "unknown"),
        );
    });
  }
  request<M extends OperationName>(
    method: M,
    params: Params<M>,
    options: RequestOptions = {},
  ): Promise<Result<M>> {
    if (this.ended || this.socket.readyState !== WebSocket.OPEN)
      return Promise.reject(
        new IvyError(
          "service_unavailable",
          "Hive service connection is unavailable.",
        ),
      );
    try {
      options.signal?.throwIfAborted();
    } catch (error) {
      return Promise.reject(error);
    }
    if (this.pending.size >= connectionInFlightRequests)
      return Promise.reject(
        new IvyError(
          "limit_exceeded",
          "Service in-flight request limit reached.",
        ),
      );
    const id = randomUUID();
    return new Promise<Result<M>>((resolve, reject) => {
      const abort = () => {
        const pending = this.pending.get(id);
        if (!pending) return;
        clearTimeout(pending.timer);
        pending.cleanup();
        this.pending.delete(id);
        reject(
          new IvyError(
            "outcome_unknown",
            "Request observation was cancelled; its execution is not presumed cancelled.",
            "unknown",
          ),
        );
      };
      const timer = setTimeout(() => {
        const pending = this.pending.get(id);
        if (!pending) return;
        this.pending.delete(id);
        pending.cleanup();
        // A deadline ends this observation; other requests and accepted work
        // still belong to the live connection. Late replies use retired IDs.
        pending.reject(
          new IvyError(
            "deadline_exceeded",
            "Hive request timed out; reconcile its operation identity.",
            "unknown",
          ),
        );
      }, options.timeoutMs ?? 35_000);
      this.pending.set(id, {
        resolve: (value) => resolve(value as Result<M>),
        reject,
        timer,
        cleanup: () => options.signal?.removeEventListener("abort", abort),
      });
      options.signal?.addEventListener("abort", abort, { once: true });
      try {
        this.send({ jsonrpc: "2.0", id, method, params });
      } catch (error) {
        const item = this.pending.get(id);
        if (item) {
          clearTimeout(item.timer);
          item.cleanup();
          this.pending.delete(id);
        }
        reject(error);
      }
    });
  }
  notification(
    namespace: string,
    name: string,
    version: string,
    payload: Wire.Json,
  ): void {
    const frame = {
      jsonrpc: "2.0",
      method: "service.notification",
      params: { namespace, name, version, payload },
    };
    this.send(frame);
  }
  private eventsAvailable(value: Transport.EventAvailability): void {
    const throughSequence = value.params.throughSequence;
    this.eventsAvailableThroughSequence = Math.max(
      this.eventsAvailableThroughSequence,
      throughSequence,
    );
    for (const waiter of [...this.eventWaiters])
      if (this.eventsAvailableThroughSequence > waiter.afterSequence)
        waiter.finish();
  }
  waitForEvents(afterSequence: number, timeoutMs: number): Promise<void> {
    if (this.eventsAvailableThroughSequence > afterSequence)
      return Promise.resolve();
    return new Promise<void>((resolve, reject) => {
      let waiter!: EventWaiter;
      const finish = (error?: unknown) => {
        clearTimeout(timer);
        this.signal.removeEventListener("abort", abort);
        this.eventWaiters.delete(waiter);
        if (error) reject(error);
        else resolve();
      };
      const abort = () =>
        finish(
          this.signal.reason ??
            new IvyError(
              "service_unavailable",
              "Hive connection is unavailable.",
              "unknown",
            ),
        );
      const timer = setTimeout(() => finish(), Math.max(25, timeoutMs));
      waiter = { afterSequence, finish };
      this.eventWaiters.add(waiter);
      this.signal.addEventListener("abort", abort, { once: true });
      if (this.eventsAvailableThroughSequence > afterSequence) finish();
    });
  }
  private async invoke(value: unknown): Promise<void> {
    let id: string | null = null,
      entered = false,
      started = false;
    try {
      validateProviderCallFrame(value);
      const frame = value as Transport.ProviderCall;
      id = frame.id;
      requireThat(
        !this.ended &&
          this.ready &&
          frame.params.generation === this.generation,
        "stale_generation",
        "Invocation generation is not ready.",
      );
      const binding = this.bindings.get(frame.params.qualifiedName);
      requireThat(
        binding && binding.hash === frame.params.definitionHash,
        "tool_definition_changed",
        "Invocation does not match this registered definition.",
      );
      this.owner.enterInvocation();
      entered = true;
      const { callerPrincipalId, operationId, generation } = frame.params;
      const context = {
        callerPrincipalId,
        generation,
        signal: this.signal,
        ...(operationId === undefined ? {} : { operationId }),
      };
      started = true;
      const result = await binding.handler(frame.params.arguments, context);
      if (!this.ended) this.send({ jsonrpc: "2.0", id, result });
    } catch (error) {
      if (id !== null && !this.ended) {
        const failure =
          error instanceof IvyError
            ? error
            : new IvyError(
                "internal_error",
                "Service invocation failed.",
                started ? "unknown" : "not_executed",
              );
        try {
          this.send({ jsonrpc: "2.0", id, error: failure.toWire() });
        } catch {
          this.close();
        }
      }
    } finally {
      if (entered) this.owner.leaveInvocation();
    }
  }
  close(
    error = new IvyError(
      "service_unavailable",
      "Service connection is stopping.",
      "unknown",
    ),
  ): void {
    if (this.ended) return;
    this.ended = true;
    this.ready = false;
    this.controller.abort(error);
    this.openReject(error);
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.cleanup();
      pending.reject(error);
    }
    this.pending.clear();
    this.socket.terminate();
    this.finish();
  }
}

export class ServiceClient {
  private readonly controller = new AbortController();
  private task: Promise<void> | null = null;
  private current: ServiceConnection | null = null;
  private acknowledged: ServiceConnection | null = null;
  private invocations = 0;
  constructor(readonly options: ServiceOptions) {
    baseUrl(options.publicBaseUrl);
    validateShared("ServiceConnect", options.identity);
  }
  get connection(): ServiceConnection {
    requireThat(
      this.ready,
      "service_not_ready",
      "The service has not reconciled its current Hive generation.",
    );
    return this.current!;
  }
  get ready(): boolean {
    return !!this.current?.ready && this.acknowledged === this.current;
  }
  start(): void {
    if (!this.task && !this.controller.signal.aborted) this.task = this.run();
  }
  async stop(): Promise<void> {
    this.controller.abort();
    this.current?.close();
    await this.task;
  }
  async waitReady(options: RequestOptions = {}): Promise<ServiceConnection> {
    const signal = AbortSignal.any([
      this.controller.signal,
      AbortSignal.timeout(options.timeoutMs ?? 30_000),
      ...(options.signal ? [options.signal] : []),
    ]);
    while (!this.ready) await delay(25, undefined, { signal });
    return this.connection;
  }
  enterInvocation(): void {
    requireThat(
      this.invocations < connectionInFlightRequests,
      "limit_exceeded",
      "Service handler capacity is full.",
    );
    this.invocations++;
  }
  leaveInvocation(): void {
    this.invocations--;
  }
  notify(frame: Transport.ProviderNotification): void {
    try {
      this.options.onNotification?.(frame);
    } catch {
      /* An observer cannot break protocol processing. */
    }
  }
  private state(
    status: Parameters<NonNullable<ServiceOptions["onState"]>>[0]["status"],
    code?: string,
    message?: string,
    phase?: string,
  ): void {
    try {
      this.options.onState?.({
        status,
        ...(this.current?.generation
          ? { generation: this.current.generation }
          : {}),
        ...(code ? { code } : {}),
        ...(message ? { message: message.slice(0, 256) } : {}),
        ...(phase ? { phase } : {}),
      });
    } catch {
      /* Observability is not control flow. */
    }
  }
  private async run(): Promise<void> {
    let failures = 0;
    const signal = this.controller.signal;
    while (!signal.aborted) {
      let connection: ServiceConnection | null = null;
      let phase = "credential";
      const stopped = () => connection?.close();
      try {
        this.state("connecting");
        const url = new URL("ws", baseUrl(this.options.publicBaseUrl));
        url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
        const credential = await dependency(this.options.credential(), signal);
        if (signal.aborted) break;
        connection = new ServiceConnection(url, credential, this);
        this.current = connection;
        signal.addEventListener("abort", stopped, { once: true });
        phase = "socket.open";
        await connection.opened;
        phase = "service.connect";
        const connected = await connection.request(
          "service.connect",
          this.options.identity,
        );
        connection.generation = connected.generation;
        this.state("syncing");
        phase = "registry.prepare";
        const registry = await connection.prepareRegistry(
          await dependency(this.options.registry(), connection.signal),
        );
        phase = "registry.sync";
        await connection.request("registry.sync", registry);
        if (this.options.onNotification) {
          phase = "notifications.subscribe";
          const filters = await dependency(
            this.options.notificationFilters?.() ?? [],
            connection.signal,
          );
          await connection.request("notifications.subscribe", { filters });
        }
        phase = "reconcile";
        await dependency(this.options.reconcile(connection), connection.signal);
        while (!connection.signal.aborted && !signal.aborted) {
          phase = "readiness";
          const observation = await dependency(
            this.options.readiness?.(connection) ?? {
              ready: true,
              diagnostics: [],
            },
            connection.signal,
          );
          connection.ready = observation.ready;
          phase = "service.heartbeat";
          const accepted = await connection.request(
            "service.heartbeat",
            observation,
          );
          connection.ready = accepted.ready;
          this.acknowledged = accepted.ready ? connection : null;
          this.state(accepted.ready ? "ready" : "degraded");
          if (accepted.ready) failures = 0;
          phase = "heartbeat.wait";
          await delay(this.options.heartbeatMs ?? 5000, undefined, {
            signal: connection.signal,
          });
        }
      } catch (error) {
        // timers/promises wraps signal.reason in AbortError during heartbeat
        // sleep. Keep the connection's actual failure visible to its owner.
        const failure =
          connection?.signal.reason instanceof IvyError
            ? connection.signal.reason
            : error;
        this.state(
          "offline",
          failure instanceof IvyError ? failure.code : "dependency_unavailable",
          failure instanceof IvyError ? failure.message : undefined,
          phase,
        );
      } finally {
        signal.removeEventListener("abort", stopped);
        connection?.close();
        if (this.current === connection) this.current = null;
      }
      if (signal.aborted) break;
      const minimum = Math.max(25, this.options.reconnectMinMs ?? 500),
        maximum = Math.max(minimum, this.options.reconnectMaxMs ?? 10_000);
      const backoff =
        Math.min(maximum, minimum * 2 ** Math.min(failures++, 8)) *
        (0.8 + Math.random() * 0.2);
      try {
        await delay(backoff, undefined, { signal });
      } catch {
        break;
      }
    }
    this.state("stopped");
  }
}

/** Resolve persistBatch only after its durable result commits. A failure explicitly withholds ack. */
export async function consumeEvents(
  connection: ServiceConnection,
  subscription: Params<"events.subscribe">,
  persistBatch: (
    batch: Result<"events.subscribe">,
    signal: AbortSignal,
  ) => Promise<void>,
  options: {
    pollMs?: number;
    onFailure?: (error: IvyError, batch: Result<"events.subscribe">) => void;
  } = {},
): Promise<void> {
  const signal = connection.signal;
  while (!signal.aborted) {
    const batch = await connection.request("events.subscribe", subscription);
    try {
      await persistBatch(batch, signal);
    } catch (error) {
      options.onFailure?.(IvyError.from(error), batch);
      throw error;
    }
    signal.throwIfAborted();
    await connection.request("events.ack", {
      name: subscription.name,
      throughSequence: batch.throughSequence,
      ...(batch.gap
        ? { gapThroughSequence: batch.gap.prunedThroughSequence }
        : {}),
    });
    if (!batch.hasMore)
      await connection.waitForEvents(
        batch.throughSequence,
        Math.max(25, options.pollMs ?? 30_000) * (0.9 + Math.random() * 0.2),
      );
  }
}
