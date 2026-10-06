import {
  digest,
  IvyError,
  requireThat,
} from "../../../../packages/sdk/src/node.js";
import type {
  Operation,
  TaskBoard,
  Wire,
} from "../../../../packages/sdk/src/node.js";
import type { TaskBoardStore } from "./store.js";

interface CategoryEntry {
  revision: number;
  createdAt: string;
  archived: boolean;
  category: string | null;
}

const categoryOf = (values: Record<string, unknown>): string | null => {
  const value = values["data:/fields/category"];
  return typeof value === "string" && value.length > 0 ? value : null;
};

const createdAtOf = (values: Record<string, unknown>): string => {
  const value = values["data:/createdAt"];
  requireThat(
    typeof value === "string" && Number.isFinite(Date.parse(value)),
    "task_board_category_projection_invalid",
    "A projected Task needs its canonical creation time.",
  );
  return value;
};

/** Disposable Task category index. Hive Tasks and their event journal remain authoritative. */
export class TaskBoardCategoryProjection {
  private readonly entries = new Map<string, CategoryEntry>();
  private readonly subscription: Wire.EventSubscribe;
  private ready = false;
  private work: Promise<void> = Promise.resolve();
  private pendingWork = 0;

  constructor(
    readonly store: TaskBoardStore,
    principalId: string,
  ) {
    const scope = digest(
      principalId + ":" + (store.rootObjectId ?? "root"),
    ).slice("sha256:".length, 31);
    this.subscription = {
      name: "task-board-categories-" + scope,
      filter: { topics: ["hive.object.changed"] },
      initialSequence: 0,
      limit: 100,
    };
  }

  private exclusive(work: () => Promise<void>): Promise<void> {
    this.pendingWork++;
    const run = async () => {
        try {
          await work();
        } finally {
          this.pendingWork--;
        }
      },
      next = this.work.then(run, run);
    this.work = next.catch(() => undefined);
    return next;
  }

  private immediate(work: () => Promise<void>): Promise<void> {
    if (this.pendingWork !== 0)
      throw new IvyError(
        "task_board_category_projection_pending",
        "The Task category projection is already aligning outside the publication gate.",
        "not_executed",
      );
    this.pendingWork = 1;
    const next = (async () => {
      try {
        await work();
      } finally {
        this.pendingWork--;
      }
    })();
    this.work = next.catch(() => undefined);
    return next;
  }

  private async rebuild(): Promise<void> {
    const replacement = new Map<string, CategoryEntry>();
    let cursor: string | undefined;
    do {
      const page = await this.store.page("task-board/task", {
        limit: 200,
        select: ["data:/fields/category", "data:/createdAt"],
        orderBy: [{ field: "object.id", direction: "asc" }],
        ...(cursor ? { cursor } : {}),
      });
      for (const item of page.items) {
        const values = item.values as Record<string, unknown>;
        replacement.set(item.objectId, {
          revision: item.revision,
          createdAt: createdAtOf(values),
          archived: false,
          category: categoryOf(values),
        });
      }
      cursor = page.nextCursor ?? undefined;
    } while (cursor);
    this.entries.clear();
    for (const [id, entry] of replacement) this.entries.set(id, entry);
    this.ready = true;
  }

  private async apply(batch: Operation.EventBatch): Promise<void> {
    if (batch.gap) {
      this.ready = false;
      await this.rebuild();
      return;
    }
    const ids = [
      ...new Set(
        batch.items.flatMap((event) => {
          const payload = event.payload as Record<string, unknown>;
          return event.topic === "hive.object.changed" &&
            typeof payload["objectId"] === "string"
            ? [payload["objectId"]]
            : [];
        }),
      ),
    ];
    if (!ids.length) return;
    const found = new Set<string>();
    for (let offset = 0; offset < ids.length; offset += 100) {
      const selected = ids.slice(offset, offset + 100);
      const page = await this.store.page("task-board/task", {
        limit: selected.length,
        where: { op: "in", field: "object.id", value: selected },
        select: ["data:/fields/category", "data:/createdAt"],
        orderBy: [{ field: "object.id", direction: "asc" }],
      });
      requireThat(
        !page.nextCursor,
        "task_board_category_projection_invalid",
        "A bounded Task event lookup must fit one page.",
      );
      for (const item of page.items) {
        found.add(item.objectId);
        const prior = this.entries.get(item.objectId);
        if (prior && prior.revision > item.revision) continue;
        const values = item.values as Record<string, unknown>;
        this.entries.set(item.objectId, {
          revision: item.revision,
          createdAt: createdAtOf(values),
          archived: false,
          category: categoryOf(values),
        });
      }
    }
    for (const id of ids) {
      if (found.has(id) || !this.entries.has(id)) continue;
      try {
        const metadata = await this.store.client.request("objects.stat", {
          objectId: id,
        });
        const prior = this.entries.get(id)!;
        if (
          metadata.contractKey === "task-board/task" &&
          metadata.parentId === this.store.rootObjectId &&
          metadata.effectivelyArchived
        ) {
          this.entries.set(id, {
            ...prior,
            revision: metadata.currentRevision,
            archived: true,
          });
        } else this.entries.delete(id);
      } catch (error) {
        if (!(error instanceof IvyError && error.code === "not_found"))
          throw error;
        this.entries.delete(id);
      }
    }
  }

  private async catchUp(
    initial?: Operation.EventBatch,
    snapshotCoversInitialGap = false,
    maximumBatches = Number.POSITIVE_INFINITY,
    rebuildGaps = true,
  ): Promise<void> {
    const { throughSequence: targetSequence } = await this.store.client.request(
      "events.head",
      {},
    );
    let previousSequence = -1;
    for (let count = 0; count < maximumBatches; count++) {
      const batch =
        count === 0 && initial
          ? initial
          : await this.store.client.request(
              "events.subscribe",
              this.subscription,
            );
      if (batch.gap && !rebuildGaps)
        throw new IvyError(
          "task_board_category_projection_pending",
          "The Task category projection needs an out-of-gate rebuild.",
          "not_executed",
        );
      if (!(count === 0 && snapshotCoversInitialGap && batch.gap))
        await this.apply(batch);
      await this.store.client.request("events.ack", {
        name: this.subscription.name,
        throughSequence: batch.throughSequence,
        ...(batch.gap
          ? { gapThroughSequence: batch.gap.prunedThroughSequence }
          : {}),
      });
      if (batch.throughSequence >= targetSequence) return;
      requireThat(batch.throughSequence > previousSequence, "task_board_cursor_regression",
        "The Task category journal did not advance toward its captured boundary.");
      previousSequence = batch.throughSequence;
    }
    throw new IvyError(
      "task_board_category_projection_pending",
      "The Task category projection could not reach its finite replay boundary within this request.",
      "not_executed",
    );
  }

  private async warmUnlocked(): Promise<void> {
    if (this.ready) return;
    // Establish the durable replay boundary before the authoritative snapshot.
    const first = await this.store.client.request(
      "events.subscribe",
      this.subscription,
    );
    await this.rebuild();
    await this.catchUp(first, true);
  }

  private async current(): Promise<void> {
    await this.exclusive(async () => {
      if (!this.ready) await this.warmUnlocked();
      else await this.catchUp();
    });
  }

  observe(objectId: string, revision: number, task: TaskBoard.Task): void {
    if (!this.ready) return;
    const prior = this.entries.get(objectId);
    if (prior && prior.revision > revision) return;
    this.entries.set(objectId, {
      revision,
      createdAt: task.createdAt,
      archived: false,
      category: task.fields.category,
    });
  }

  private values(): string[] {
    requireThat(
      this.ready,
      "task_board_category_projection_pending",
      "The Task category projection is rebuilding.",
    );
    const first = new Map<
      string,
      { value: string; createdAt: string; objectId: string }
    >();
    for (const [objectId, entry] of this.entries) {
      if (entry.archived || !entry.category) continue;
      const key = entry.category.toLocaleLowerCase(),
        prior = first.get(key);
      if (
        !prior ||
        entry.createdAt < prior.createdAt ||
        (entry.createdAt === prior.createdAt && objectId < prior.objectId)
      ) {
        first.set(key, {
          value: entry.category,
          createdAt: entry.createdAt,
          objectId,
        });
      }
    }
    return [...first.values()]
      .map((value) => value.value)
      .sort((left, right) => left.localeCompare(right))
      .slice(0, 256);
  }

  async categories(): Promise<TaskBoard.CategoriesResult> {
    await this.current();
    return { categories: this.values() };
  }

  async normalize(category: string | null): Promise<string | null> {
    if (category === null) return null;
    await this.current();
    return this.normalized(category);
  }

  /** Recheck only the bounded journal edge while the publication gate is held. */
  async revalidate(category: string): Promise<string> {
    await this.immediate(async () => {
      requireThat(
        this.ready,
        "task_board_category_projection_pending",
        "The Task category projection needs an out-of-gate rebuild.",
      );
      await this.catchUp(undefined, false, 1, false);
    });
    return this.normalized(category);
  }

  private normalized(category: string): string {
    const existing = this.values().find(
      (value) => value.toLocaleLowerCase() === category.toLocaleLowerCase(),
    );
    return existing ?? category;
  }
}
