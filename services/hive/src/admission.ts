import {
  IvyError,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import { consumerInFlightRequests } from "../../../packages/contracts/src/limits.js";

export interface RequestLease {
  grow(bytes: number): void;
  release(): void;
}
/** Fixed-size, lazy-expiring admission table; rejected traffic never creates an unbounded queue. */
export class AuthenticationRate {
  private global = { at: 0, count: 0 };
  private readonly clients = new Map<string, { at: number; count: number }>();
  constructor(
    readonly windowMs = 1000,
    readonly maximum = 512,
    readonly perClient = 256,
    readonly clientMaximum = 4096,
  ) {}
  admit(key: string, now = Date.now()): void {
    if (now - this.global.at >= this.windowMs)
      this.global = { at: now, count: 0 };
    let client = this.clients.get(key);
    if (!client || now - client.at >= this.windowMs) {
      for (const [id, entry] of this.clients)
        if (now - entry.at >= this.windowMs) this.clients.delete(id);
      requireThat(
        this.clients.size < this.clientMaximum,
        "limit_exceeded",
        "Authentication client budget is full.",
      );
      client = { at: now, count: 0 };
      this.clients.set(key, client);
    }
    if (this.global.count >= this.maximum || client.count >= this.perClient) {
      const global = this.global.count >= this.maximum;
      throw new IvyError(
        "limit_exceeded",
        "Authentication arrival budget is full.",
        "not_executed",
        {
          budget: "authentication_arrivals",
          scope: global ? "global" : "client",
          active: global ? this.global.count : client.count,
          limit: global ? this.maximum : this.perClient,
          windowMs: this.windowMs,
        },
      );
    }
    this.global.count++;
    client.count++;
  }
}
/** Retained input remains charged through queueing, execution and provider completion. */
export class RequestBudget {
  private count = 0;
  private bytes = 0;
  private readonly consumers = new Map<
    string,
    { count: number; bytes: number }
  >();
  constructor(
    readonly maximum = 2048,
    readonly maximumBytes = 128 * 1024 * 1024,
    readonly perConsumer = consumerInFlightRequests,
    readonly perConsumerBytes = 64 * 1024 * 1024,
    readonly name = "request",
  ) {}
  acquire(key: string, bytes = 1024): RequestLease {
    const consumer = this.consumers.get(key) ?? { count: 0, bytes: 0 };
    if (this.count >= this.maximum || consumer.count >= this.perConsumer) {
      const global = this.count >= this.maximum;
      throw new IvyError(
        "limit_exceeded",
        `Hive ${this.name} slots are full.`,
        "not_executed",
        {
          budget: this.name,
          scope: global ? "global" : "consumer",
          active: global ? this.count : consumer.count,
          limit: global ? this.maximum : this.perConsumer,
        },
      );
    }
    let held = 0,
      released = false;
    const grow = (amount: number) => {
      requireThat(
        !released && Number.isSafeInteger(amount) && amount >= 0,
        "invalid_arguments",
        "Invalid request reservation.",
      );
      if (
        this.bytes + amount > this.maximumBytes ||
        consumer.bytes + amount > this.perConsumerBytes
      ) {
        const global = this.bytes + amount > this.maximumBytes;
        throw new IvyError(
          "limit_exceeded",
          `Hive retained ${this.name} bytes are full.`,
          "not_executed",
          {
            budget: this.name + "_bytes",
            scope: global ? "global" : "consumer",
            active: global ? this.bytes : consumer.bytes,
            requested: amount,
            limit: global ? this.maximumBytes : this.perConsumerBytes,
          },
        );
      }
      held += amount;
      this.bytes += amount;
      consumer.bytes += amount;
    };
    grow(bytes);
    this.count++;
    consumer.count++;
    this.consumers.set(key, consumer);
    return {
      grow,
      release: () => {
        if (released) return;
        released = true;
        this.count--;
        consumer.count--;
        this.bytes -= held;
        consumer.bytes -= held;
        if (!consumer.count) this.consumers.delete(key);
      },
    };
  }
}

interface Waiting {
  start(): void;
  reject(error: Error): void;
  timer: NodeJS.Timeout;
}
/** Expired work is removed, not left captured by an unbounded Promise chain. */
export class DispatchGate {
  private active = false;
  private readonly waiting: Waiting[] = [];
  constructor(
    readonly maximum = 1024,
    readonly waitMs = 30_000,
  ) {}
  run<T>(work: () => Promise<T>): Promise<T> {
    if (this.waiting.length + Number(this.active) >= this.maximum)
      return Promise.reject(
        new IvyError(
          "limit_exceeded",
          "Hive dispatch slots are full.",
          "not_executed",
          {
            budget: "dispatch",
            active: this.waiting.length + Number(this.active),
            limit: this.maximum,
          },
        ),
      );
    return new Promise<T>((resolve, reject) => {
      const entry: Waiting = {
        reject,
        timer: undefined!,
        start: () => {
          clearTimeout(entry.timer);
          this.active = true;
          void Promise.resolve()
            .then(work)
            .then(resolve, reject)
            .finally(() => {
              this.active = false;
              this.waiting.shift()?.start();
            });
        },
      };
      entry.timer = setTimeout(() => {
        const index = this.waiting.indexOf(entry);
        if (index < 0) return;
        this.waiting.splice(index, 1);
        reject(
          new IvyError(
            "deadline_exceeded",
            "Hive dispatch wait expired before execution.",
            "not_executed",
          ),
        );
      }, this.waitMs);
      if (this.active) this.waiting.push(entry);
      else entry.start();
    });
  }
}
