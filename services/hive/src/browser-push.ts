import webPush from "web-push";
import type { WorkerClient } from "./worker-client.js";
import type { PushBatch } from "./browser-push-store.js";

export type PushSender = typeof webPush.sendNotification;

/** A bounded sender shared by service events and the test button. */
export class BrowserPush {
  private running: Promise<void> | null = null;
  private requested = false;
  private stopped = false;
  private timer: NodeJS.Timeout | null = null;
  constructor(
    readonly worker: WorkerClient,
    readonly subject: string,
    readonly send: PushSender = webPush.sendNotification,
  ) {}
  start(): void {
    this.timer = setInterval(() => this.wake(), 15_000);
    this.timer.unref();
    this.wake();
  }
  wake(): void {
    this.requested = true;
    if (this.running || this.stopped) return;
    this.running = this.drain()
      .catch(() => {
        // Storage recovery retries through the timer. Delivery errors are retained
        // against the browser subscription without exposing its endpoint or keys.
      })
      .finally(() => {
        this.running = null;
        if (this.requested && !this.stopped) this.wake();
      });
  }
  private async drain(): Promise<void> {
    do {
      this.requested = false;
      const batch = await this.worker.request<PushBatch>({
        action: "browserPush.next",
      });
      this.requested ||= batch.hasMore;
      const jobs = batch.deliveries;
      if (!jobs.length) continue;
      const keys = await this.worker.request<webPush.VapidKeys>({
        action: "browserPush.keys",
      });
      await Promise.all(
        jobs.map(async (job) => {
          let status = 0;
          try {
            const result = await this.send(
              job.subscription,
              JSON.stringify(job.notice),
              {
                vapidDetails: { ...keys, subject: this.subject },
                TTL: 3600,
                timeout: 10_000,
              },
            );
            status = result.statusCode;
          } catch (error) {
            status = (error as { statusCode?: number }).statusCode ?? 0;
          }
          await this.worker.request({
            action: "browserPush.complete",
            id: job.id,
            status,
          });
        }),
      );
      this.requested = true;
    } while (this.requested && !this.stopped);
  }
  async close(): Promise<void> {
    this.stopped = true;
    if (this.timer) clearInterval(this.timer);
    await this.running;
  }
}
