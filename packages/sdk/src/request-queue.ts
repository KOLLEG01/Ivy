import { IvyError, requireThat } from "../../contracts/src/errors.js";

interface WaitingRequest {
  provider: boolean;
  signal: AbortSignal;
  start(): void;
}

/** Admission only: each request is sent once, and cancelled waiting work is removed. */
export class RequestQueue {
  private active = 0;
  private providers = 0;
  private readonly waiting: WaitingRequest[] = [];
  constructor(
    readonly maximum: number,
    readonly providerMaximum: number,
    readonly waitingMaximum = 128,
  ) {
    requireThat(
      [maximum, providerMaximum, waitingMaximum].every(
        (value) => Number.isSafeInteger(value) && value > 0,
      ) && providerMaximum <= maximum,
      "invalid_arguments",
      "Invalid client request admission limits.",
    );
  }
  acquire(provider: boolean, signal: AbortSignal): Promise<() => void> {
    signal.throwIfAborted();
    const available = () =>
      this.active < this.maximum &&
      (!provider || this.providers < this.providerMaximum);
    if (available()) return Promise.resolve(this.enter(provider));
    if (this.waiting.length >= this.waitingMaximum)
      return Promise.reject(
        new IvyError(
          "limit_exceeded",
          "Hive client request queue is full.",
          "not_executed",
          {
            budget: "client_queue",
            active: this.waiting.length,
            limit: this.waitingMaximum,
          },
        ),
      );
    return new Promise((resolve, reject) => {
      const abort = () => {
        const index = this.waiting.indexOf(entry);
        if (index >= 0) this.waiting.splice(index, 1);
        reject(signal.reason);
      };
      const entry: WaitingRequest = {
        provider,
        signal,
        start: () => {
          signal.removeEventListener("abort", abort);
          resolve(this.enter(provider));
        },
      };
      signal.addEventListener("abort", abort, { once: true });
      this.waiting.push(entry);
    });
  }
  private enter(provider: boolean): () => void {
    this.active++;
    if (provider) this.providers++;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.active--;
      if (provider) this.providers--;
      // Slow providers cannot prevent ordinary reads from using the remaining capacity.
      while (this.active < this.maximum) {
        const index = this.waiting.findIndex(
          (entry) => !entry.provider || this.providers < this.providerMaximum,
        );
        if (index < 0) break;
        this.waiting.splice(index, 1)[0]!.start();
      }
    };
  }
}
