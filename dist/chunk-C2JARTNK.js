import {
  wireSchema
} from "./chunk-G7GHB54E.js";
import {
  baseUrl,
  responseValue,
  validateEventAvailabilityFrame,
  validateProviderCallFrame,
  validateProviderNotificationFrame
} from "./chunk-CC77VDAO.js";
import {
  hashJson
} from "./chunk-KI5VCYRK.js";
import {
  IvyError,
  connectionInFlightRequests,
  encodeJson,
  managementFrameBytes,
  requireThat
} from "./chunk-BO4WKKA7.js";

// packages/sdk/src/service.ts
import { WebSocket } from "ws";
import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";

// packages/contracts/src/tool-definition.ts
function toolDefinitionHash(definition) {
  const { discovery: _discovery, ...callable } = definition;
  return hashJson(callable);
}

// packages/contracts/src/wire-validation.ts
import { Ajv2020 } from "ajv/dist/2020.js";
var ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
ajv.addSchema(wireSchema);
var validators = /* @__PURE__ */ new Map();
function validateShared(name, value) {
  let validator = validators.get(name);
  if (!validator) {
    validator = ajv.getSchema(`${String(wireSchema["$id"])}#/$defs/${name}`);
    requireThat(validator, "internal_error", "Unknown shared contract.");
    validators.set(name, validator);
  }
  requireThat(validator(value), "invalid_arguments", "Value does not match its shared contract.");
}

// packages/sdk/src/service.ts
async function dependency(value, signal) {
  signal.throwIfAborted();
  return new Promise((resolve, reject) => {
    const abort = () => {
      cleanup();
      reject(
        new IvyError(
          "service_unavailable",
          "Dependency observation was cancelled; accepted work must be reconciled.",
          "unknown"
        )
      );
    };
    const timer = setTimeout(() => {
      cleanup();
      reject(
        new IvyError(
          "deadline_exceeded",
          "Service dependency did not finish before its deadline.",
          "unknown"
        )
      );
    }, 3e4);
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
      }
    );
  });
}
var ServiceConnection = class {
  constructor(url, credential, owner) {
    this.owner = owner;
    this.closed = new Promise((resolve) => {
      this.finish = resolve;
    });
    const origin = new URL(owner.options.publicBaseUrl).origin;
    this.socket = new WebSocket(url, {
      headers: { Authorization: "Bearer " + credential, Origin: origin },
      handshakeTimeout: 1e4,
      maxPayload: managementFrameBytes,
      perMessageDeflate: false
    });
    this.opened = new Promise((resolve, reject) => {
      this.openReject = reject;
      this.socket.once("open", () => resolve());
    });
    this.socket.on(
      "error",
      () => this.close(
        new IvyError(
          "service_unavailable",
          "Hive WebSocket failed.",
          "unknown"
        )
      )
    );
    this.socket.on(
      "close",
      () => this.close(
        new IvyError(
          "outcome_unknown",
          "Hive connection closed; reconcile accepted operations.",
          "unknown"
        )
      )
    );
    this.socket.on("message", (bytes, binary) => {
      if (binary) {
        this.close(
          new IvyError(
            "invalid_frame",
            "Hive sent a binary protocol frame.",
            "unknown"
          )
        );
        return;
      }
      try {
        const frame = JSON.parse(
          new TextDecoder("utf-8", { fatal: true }).decode(
            Buffer.isBuffer(bytes) ? bytes : Buffer.concat(bytes)
          )
        );
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
        const id = String(frame["id"]), pending = this.pending.get(id);
        if (!pending) return;
        this.pending.delete(id);
        clearTimeout(pending.timer);
        pending.cleanup();
        try {
          pending.resolve(responseValue(frame, id));
        } catch (error) {
          pending.reject(
            error instanceof IvyError ? error : new IvyError(
              "invalid_frame",
              "Invalid Hive result.",
              "unknown"
            )
          );
        }
      } catch {
        this.close(
          new IvyError(
            "invalid_frame",
            "Hive sent malformed protocol data.",
            "unknown"
          )
        );
      }
    });
  }
  owner;
  socket;
  controller = new AbortController();
  closed;
  opened;
  generation = 0;
  get serviceNodeId() {
    return this.owner.options.identity.serviceNodeId;
  }
  ready = false;
  pending = /* @__PURE__ */ new Map();
  eventWaiters = /* @__PURE__ */ new Set();
  eventsAvailableThroughSequence = 0;
  bindings = /* @__PURE__ */ new Map();
  finish;
  openReject;
  ended = false;
  get signal() {
    return this.controller.signal;
  }
  registryBindings(registry) {
    const bindings = /* @__PURE__ */ new Map();
    for (const namespace of registry.namespaces)
      for (const definition of namespace.tools) {
        const name = definition.namespace + "." + definition.name;
        const objectHandler = Object.hasOwn(this.owner.options.handlers, name), jsonHandler = Object.hasOwn(
          this.owner.options.jsonHandlers ?? {},
          name
        );
        requireThat(
          !bindings.has(name) && objectHandler !== jsonHandler && typeof (objectHandler ? this.owner.options.handlers[name] : this.owner.options.jsonHandlers?.[name]) === "function",
          "registry_invalid",
          "Every declared tool needs exactly one concrete object or JSON handler."
        );
        const handler = jsonHandler ? this.owner.options.jsonHandlers[name] : this.owner.options.handlers[name];
        bindings.set(name, {
          hash: toolDefinitionHash(definition),
          handler
        });
      }
    return bindings;
  }
  registrySnapshot(registry) {
    validateShared("RegistrySync", registry);
    const snapshot = structuredClone(registry);
    const freeze = (value) => {
      if (value && typeof value === "object") {
        for (const child of Object.values(value)) freeze(child);
        Object.freeze(value);
      }
    };
    freeze(snapshot);
    return snapshot;
  }
  setRegistry(registry) {
    this.bindings = this.registryBindings(this.registrySnapshot(registry));
  }
  async prepareRegistry(registry) {
    this.signal.throwIfAborted();
    const snapshot = this.registrySnapshot(registry), bindings = this.registryBindings(snapshot);
    this.bindings = bindings;
    return snapshot;
  }
  send(value) {
    requireThat(
      !this.ended && this.socket.readyState === WebSocket.OPEN,
      "service_unavailable",
      "Hive connection is unavailable."
    );
    requireThat(
      this.socket.bufferedAmount <= 1024 * 1024,
      "limit_exceeded",
      "Service socket buffer is full."
    );
    this.socket.send(encodeJson(value), (error) => {
      if (error)
        this.close(
          new IvyError("outcome_unknown", "Hive write failed.", "unknown")
        );
    });
  }
  request(method, params, options = {}) {
    if (this.ended || this.socket.readyState !== WebSocket.OPEN)
      return Promise.reject(
        new IvyError(
          "service_unavailable",
          "Hive service connection is unavailable."
        )
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
          "Service in-flight request limit reached."
        )
      );
    const id = randomUUID();
    return new Promise((resolve, reject) => {
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
            "unknown"
          )
        );
      };
      const timer = setTimeout(
        () => this.close(
          new IvyError(
            "deadline_exceeded",
            "Hive request timed out; reconcile its operation identity.",
            "unknown"
          )
        ),
        options.timeoutMs ?? 35e3
      );
      this.pending.set(id, {
        resolve: (value) => resolve(value),
        reject,
        timer,
        cleanup: () => options.signal?.removeEventListener("abort", abort)
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
  notification(namespace, name, version, payload) {
    const frame = {
      jsonrpc: "2.0",
      method: "service.notification",
      params: { namespace, name, version, payload }
    };
    this.send(frame);
  }
  eventsAvailable(value) {
    const throughSequence = value.params.throughSequence;
    this.eventsAvailableThroughSequence = Math.max(
      this.eventsAvailableThroughSequence,
      throughSequence
    );
    for (const waiter of [...this.eventWaiters])
      if (this.eventsAvailableThroughSequence > waiter.afterSequence)
        waiter.finish();
  }
  waitForEvents(afterSequence, timeoutMs) {
    if (this.eventsAvailableThroughSequence > afterSequence)
      return Promise.resolve();
    return new Promise((resolve, reject) => {
      let waiter;
      const finish = (error) => {
        clearTimeout(timer);
        this.signal.removeEventListener("abort", abort);
        this.eventWaiters.delete(waiter);
        if (error) reject(error);
        else resolve();
      };
      const abort = () => finish(
        this.signal.reason ?? new IvyError(
          "service_unavailable",
          "Hive connection is unavailable.",
          "unknown"
        )
      );
      const timer = setTimeout(() => finish(), Math.max(25, timeoutMs));
      waiter = { afterSequence, finish };
      this.eventWaiters.add(waiter);
      this.signal.addEventListener("abort", abort, { once: true });
      if (this.eventsAvailableThroughSequence > afterSequence) finish();
    });
  }
  async invoke(value) {
    let id = null, entered = false, started = false;
    try {
      validateProviderCallFrame(value);
      const frame = value;
      id = frame.id;
      requireThat(
        !this.ended && this.ready && frame.params.generation === this.generation,
        "stale_generation",
        "Invocation generation is not ready."
      );
      const binding = this.bindings.get(frame.params.qualifiedName);
      requireThat(
        binding && binding.hash === frame.params.definitionHash,
        "tool_definition_changed",
        "Invocation does not match this registered definition."
      );
      this.owner.enterInvocation();
      entered = true;
      const { callerPrincipalId, operationId, generation } = frame.params;
      const context = {
        callerPrincipalId,
        generation,
        signal: this.signal,
        ...operationId === void 0 ? {} : { operationId }
      };
      started = true;
      const result = await binding.handler(frame.params.arguments, context);
      if (!this.ended) this.send({ jsonrpc: "2.0", id, result });
    } catch (error) {
      if (id !== null && !this.ended) {
        const failure = error instanceof IvyError ? error : new IvyError(
          "internal_error",
          "Service invocation failed.",
          started ? "unknown" : "not_executed"
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
  close(error = new IvyError(
    "service_unavailable",
    "Service connection is stopping.",
    "unknown"
  )) {
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
};
var ServiceClient = class {
  constructor(options) {
    this.options = options;
    baseUrl(options.publicBaseUrl);
    validateShared("ServiceConnect", options.identity);
  }
  options;
  controller = new AbortController();
  task = null;
  current = null;
  acknowledged = null;
  invocations = 0;
  get connection() {
    requireThat(
      this.ready,
      "service_not_ready",
      "The service has not reconciled its current Hive generation."
    );
    return this.current;
  }
  get ready() {
    return !!this.current?.ready && this.acknowledged === this.current;
  }
  start() {
    if (!this.task && !this.controller.signal.aborted) this.task = this.run();
  }
  async stop() {
    this.controller.abort();
    this.current?.close();
    await this.task;
  }
  async waitReady(options = {}) {
    const signal = AbortSignal.any([
      this.controller.signal,
      AbortSignal.timeout(options.timeoutMs ?? 3e4),
      ...options.signal ? [options.signal] : []
    ]);
    while (!this.ready) await delay(25, void 0, { signal });
    return this.connection;
  }
  enterInvocation() {
    requireThat(
      this.invocations < connectionInFlightRequests,
      "limit_exceeded",
      "Service handler capacity is full."
    );
    this.invocations++;
  }
  leaveInvocation() {
    this.invocations--;
  }
  notify(frame) {
    try {
      this.options.onNotification?.(frame);
    } catch {
    }
  }
  state(status, code, message) {
    try {
      this.options.onState?.({
        status,
        ...this.current?.generation ? { generation: this.current.generation } : {},
        ...code ? { code } : {},
        ...message ? { message: message.slice(0, 256) } : {}
      });
    } catch {
    }
  }
  async run() {
    let failures = 0;
    const signal = this.controller.signal;
    while (!signal.aborted) {
      let connection = null;
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
        await connection.opened;
        const connected = await connection.request(
          "service.connect",
          this.options.identity
        );
        connection.generation = connected.generation;
        this.state("syncing");
        const registry = await connection.prepareRegistry(
          await dependency(this.options.registry(), connection.signal)
        );
        await connection.request("registry.sync", registry);
        if (this.options.onNotification) {
          const filters = await dependency(
            this.options.notificationFilters?.() ?? [],
            connection.signal
          );
          await connection.request("notifications.subscribe", { filters });
        }
        await dependency(this.options.reconcile(connection), connection.signal);
        while (!connection.signal.aborted && !signal.aborted) {
          const observation = await dependency(
            this.options.readiness?.(connection) ?? {
              ready: true,
              diagnostics: []
            },
            connection.signal
          );
          connection.ready = observation.ready;
          const accepted = await connection.request(
            "service.heartbeat",
            observation
          );
          connection.ready = accepted.ready;
          this.acknowledged = accepted.ready ? connection : null;
          this.state(accepted.ready ? "ready" : "degraded");
          if (accepted.ready) failures = 0;
          await delay(this.options.heartbeatMs ?? 5e3, void 0, {
            signal: connection.signal
          });
        }
      } catch (error) {
        this.state(
          "offline",
          error instanceof IvyError ? error.code : "dependency_unavailable",
          error instanceof IvyError ? error.message : void 0
        );
      } finally {
        signal.removeEventListener("abort", stopped);
        connection?.close();
        if (this.current === connection) this.current = null;
      }
      if (signal.aborted) break;
      const minimum = Math.max(25, this.options.reconnectMinMs ?? 500), maximum = Math.max(minimum, this.options.reconnectMaxMs ?? 1e4);
      const backoff = Math.min(maximum, minimum * 2 ** Math.min(failures++, 8)) * (0.8 + Math.random() * 0.2);
      try {
        await delay(backoff, void 0, { signal });
      } catch {
        break;
      }
    }
    this.state("stopped");
  }
};
async function consumeEvents(connection, subscription, persistBatch, options = {}) {
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
      ...batch.gap ? { gapThroughSequence: batch.gap.prunedThroughSequence } : {}
    });
    if (!batch.hasMore)
      await connection.waitForEvents(
        batch.throughSequence,
        Math.max(25, options.pollMs ?? 3e4) * (0.9 + Math.random() * 0.2)
      );
  }
}

export {
  toolDefinitionHash,
  ServiceConnection,
  ServiceClient,
  consumeEvents
};
//# sourceMappingURL=chunk-C2JARTNK.js.map
