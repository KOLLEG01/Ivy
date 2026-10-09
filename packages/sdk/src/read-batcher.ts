import { canonical, encodeJson } from "../../contracts/src/canonical-json.js";
import { IvyError } from "../../contracts/src/errors.js";
import {
  rpcBatchRequests,
  rpcBatchRequestBytes,
} from "../../contracts/src/limits.js";

export interface ReadFrame {
  jsonrpc: "2.0";
  id: string;
  method: string;
  params: Record<string, unknown>;
}
interface Consumer {
  signal: AbortSignal;
  sent(): void;
  resolve(value: unknown): void;
  reject(error: unknown): void;
  abort(): void;
}
interface Read {
  key: string;
  frame: ReadFrame;
  bytes: number;
  controller: AbortController;
  consumers: Set<Consumer>;
  sent: boolean;
  release?: () => void;
  cancelBatch?: () => void;
}

/** No result cache or replay: only concurrent observations share work, with independent cancellation. */
export class ReadBatcher {
  private readonly reads = new Map<string, Read>();
  private readonly waiting: Read[] = [];
  private timer: ReturnType<typeof setTimeout> | undefined;
  constructor(
    private readonly acquire: (
      provider: boolean,
      signal: AbortSignal,
    ) => Promise<() => void>,
    private readonly exchange: (
      body: string,
      signal: AbortSignal,
    ) => Promise<unknown>,
    private readonly response: (frame: unknown, id: string) => unknown,
  ) {}
  /** A read begun after a write must not join an observation from before that write. */
  invalidate(): void {
    this.reads.clear();
  }
  request(
    method: string,
    params: Record<string, unknown>,
    signal: AbortSignal,
    sent: () => void,
  ): Promise<unknown> {
    signal.throwIfAborted();
    const key = method + "\0" + canonical(params);
    let read = this.reads.get(key);
    if (!read) {
      const frame: ReadFrame = {
        jsonrpc: "2.0",
        id: crypto.randomUUID(),
        method,
        params: structuredClone(params),
      };
      read = {
        key,
        frame,
        bytes: new TextEncoder().encode(encodeJson(frame)).length,
        controller: new AbortController(),
        consumers: new Set(),
        sent: false,
      };
      this.reads.set(key, read);
      void this.admit(read);
    }
    const selected = read;
    return new Promise((resolve, reject) => {
      const consumer: Consumer = {
        signal,
        sent,
        resolve,
        reject,
        abort: () => {
          selected.consumers.delete(consumer);
          signal.removeEventListener("abort", consumer.abort);
          reject(
            selected.sent
              ? new IvyError(
                  "outcome_unknown",
                  "The read connection failed or timed out.",
                  "unknown",
                )
              : signal.reason,
          );
          if (!selected.consumers.size)
            this.finish(selected, undefined, signal.reason);
        },
      };
      selected.consumers.add(consumer);
      signal.addEventListener("abort", consumer.abort, { once: true });
      if (selected.sent) sent();
    });
  }
  private async admit(read: Read): Promise<void> {
    try {
      read.release = await this.acquire(
        read.frame.method === "tools.call" ||
          read.frame.method === "discovery.call",
        read.controller.signal,
      );
      if (!read.consumers.size) {
        read.release();
        return;
      }
      this.waiting.push(read);
      // A short shared window gathers reads from mounted panels and coalesced live updates.
      this.timer ??= setTimeout(() => this.flush(), 10);
    } catch (error) {
      this.finish(read, undefined, error);
    }
  }
  private flush(): void {
    this.timer = undefined;
    const waiting = this.waiting
      .splice(0)
      .filter((read) => read.consumers.size);
    // A waiting provider must not hold ordinary Hive reads behind its response.
    const lanes = new Map<string, Read[]>();
    for (const read of waiting) {
      const provider =
        read.frame.method === "tools.call" ||
        read.frame.method === "discovery.call";
      const key = provider
        ? "provider:" +
          canonical({
            serviceNodeId: read.frame.params["serviceNodeId"] ?? null,
            serviceName: read.frame.params["serviceName"] ?? null,
            resourceRef: read.frame.params["resourceRef"] ?? null,
          })
        : "hive";
      const lane = lanes.get(key) ?? [];
      lane.push(read);
      lanes.set(key, lane);
    }
    for (const lane of lanes.values()) {
      let batch: Read[] = [],
        bytes = 2;
      for (const read of lane) {
        if (
          batch.length &&
          (batch.length === rpcBatchRequests ||
            bytes + read.bytes + 1 > rpcBatchRequestBytes)
        ) {
          void this.send(batch);
          batch = [];
          bytes = 2;
        }
        batch.push(read);
        bytes += read.bytes + 1;
      }
      if (batch.length) void this.send(batch);
    }
  }
  private async send(reads: Read[]): Promise<void> {
    const controller = new AbortController();
    for (const read of reads) {
      read.sent = true;
      read.cancelBatch = () => {
        if (reads.every((value) => !value.consumers.size)) controller.abort();
      };
      for (const consumer of read.consumers) consumer.sent();
    }
    try {
      const frames = reads.map((read) => read.frame);
      const result = await this.exchange(
        encodeJson(frames.length === 1 ? frames[0] : frames),
        controller.signal,
      );
      if (!Array.isArray(result)) {
        for (const read of reads) {
          try {
            // An HTTP-level refusal has a null ID and applies to every item.
            if (
              reads.length > 1 &&
              (result as { id?: unknown } | null)?.id !== null
            )
              throw new IvyError(
                "invalid_frame",
                "Hive returned no batch response.",
                "unknown",
              );
            this.finish(read, this.response(result, read.frame.id));
          } catch (error) {
            this.finish(read, undefined, error);
          }
        }
        return;
      }
      const indexed = new Map<string, unknown>();
      for (const frame of result) {
        const id = (frame as { id?: unknown } | null)?.id;
        if (
          typeof id !== "string" ||
          indexed.has(id) ||
          !reads.some((read) => read.frame.id === id)
        )
          throw new IvyError(
            "invalid_frame",
            "Hive batch response identities are invalid.",
            "unknown",
          );
        indexed.set(id, frame);
      }
      if (indexed.size !== reads.length)
        throw new IvyError(
          "invalid_frame",
          "Hive batch response is incomplete.",
          "unknown",
        );
      for (const read of reads) {
        try {
          this.finish(
            read,
            this.response(indexed.get(read.frame.id), read.frame.id),
          );
        } catch (error) {
          this.finish(read, undefined, error);
        }
      }
    } catch (error) {
      for (const read of reads) this.finish(read, undefined, error);
    }
  }
  private finish(read: Read, value?: unknown, error?: unknown): void {
    if (this.reads.get(read.key) === read) this.reads.delete(read.key);
    for (const consumer of read.consumers) {
      consumer.signal.removeEventListener("abort", consumer.abort);
      if (error !== undefined) consumer.reject(error);
      else consumer.resolve(value);
    }
    read.consumers.clear();
    read.controller.abort();
    read.release?.();
    delete read.release;
    read.cancelBatch?.();
  }
}
