import { randomUUID } from "node:crypto";
import {
  canonical,
  IvyError,
  newOperationId,
  requireThat,
  serviceTools,
} from "../../../packages/sdk/src/node.js";
import type {
  InvocationContext,
  Operation,
  Params,
  RpcClient,
  Wire,
} from "../../../packages/sdk/src/node.js";
import {
  assignmentDocument,
  definitionDocument,
  observationDocument,
  readSchema,
  scriptDocument,
  taskSchema,
  updateSchema,
} from "./schema.js";
import type { Task } from "./schema.js";
import type { CollectorEngine } from "./engine.js";
import type { RecordTask } from "./store.js";

type Document = Operation.ObjectRead;
type Pin = { objectId: string; revision: number };
const pin = (value: Document): Pin => ({
  objectId: value.object.id,
  revision: value.revision.revision,
});
const json = (value: unknown) => value as Wire.Json;
const content = (value: Document): unknown => {
  requireThat(
    value.content.encoding === "json",
    "target_conflict",
    "Collector control data must be JSON.",
  );
  return value.content.value;
};

/** Hive holds desired definitions; SQLite holds only accepted execution snapshots and journals. */
export class CollectorDefinitions {
  private tail: Promise<unknown> = Promise.resolve();
  private catalogId = "";
  private lastObserved = new Map<string, string>();
  private errors = new Map<string, string>();
  private observations: Document[] | null = null;
  constructor(
    readonly serviceNodeId: string,
    readonly engine: CollectorEngine,
  ) {}
  private get store() {
    return this.engine.store;
  }
  private hive(): RpcClient {
    requireThat(
      this.engine.client,
      "service_not_ready",
      "Hive is required to edit definitions; accepted jobs continue locally.",
    );
    return this.engine.client;
  }
  private serial<T>(work: () => Promise<T>): Promise<T> {
    const next = this.tail.catch(() => undefined).then(work);
    this.tail = next;
    return next;
  }
  private async mutation(operationId: string, step: string): Promise<string> {
    const key = "definition-operation:" + operationId + ":" + step;
    let id = this.store.metadata(key);
    if (!id) {
      id = await newOperationId(this.hive());
      this.store.setMetadata(key, id);
    }
    return id;
  }
  private async all(kind: string): Promise<Document[]> {
    const items: Document[] = [];
    let cursor: string | undefined;
    do {
      const page = await this.hive().request("objects.query", {
        contractKey: "data-collector/" + kind,
        includeContent: true,
        limit: 200,
        ...(cursor ? { cursor } : {}),
      });
      for (const item of page.items) {
        requireThat(
          item.document,
          "invalid_frame",
          "Collector query omitted its pinned document.",
        );
        items.push(item.document);
      }
      cursor = page.nextCursor ?? undefined;
    } while (cursor);
    return items;
  }
  private async document(
    kind: string,
    objectId: string,
    revision?: number,
  ): Promise<Document> {
    const value = await this.hive().request("objects.read", {
      objectId,
      ...(revision ? { revision } : {}),
    });
    requireThat(
      value.object.contractKey === "data-collector/" + kind &&
        !value.object.effectivelyArchived,
      "target_conflict",
      "Unexpected or archived collector control object.",
    );
    return value;
  }
  private async catalog() {
    if (this.catalogId) return this.catalogId;
    const found = await this.all("catalog");
    // Catalogs group control objects; queries span all catalogs. Simultaneous
    // first starts may create two empty roots without creating two authorities.
    if (found[0])
      this.catalogId = found.sort((a, b) =>
        a.object.id.localeCompare(b.object.id),
      )[0]!.object.id;
    else {
      const created = await this.hive().request("objects.write", {
        mutationId: await this.mutation("catalog", "create"),
        create: {
          name: "Collector definitions",
          parentId: null,
          ownerObjectId: null,
          contractKey: "data-collector/catalog",
        },
        contractVersion: "1.0.0",
        references: {},
        content: { encoding: "json", value: { schemaVersion: 1 } },
      });
      this.catalogId = created.object.id;
    }
    return this.catalogId;
  }
  private async write(
    kind: string,
    value: unknown,
    operationId: string,
    step: string,
    previous?: Document,
    references: Record<string, Pin> = {},
  ): Promise<Document> {
    canonical(json(value), 1048576);
    const key = "definition-write:" + operationId + ":" + step;
    const retained = kind === "observation" ? null : this.store.metadata(key);
    const request: Params<"objects.write"> = retained
      ? JSON.parse(retained)
      : {
          mutationId:
            kind === "observation"
              ? await newOperationId(this.hive())
              : await this.mutation(operationId, step),
          contractVersion: "1.0.0",
          references,
          ...(previous
            ? {
                objectId: previous.object.id,
                expectedRevision: previous.revision.revision,
              }
            : {
                create: {
                  name: kind + "-" + randomUUID(),
                  parentId: await this.catalog(),
                  ownerObjectId: null,
                  contractKey: "data-collector/" + kind,
                },
              }),
          content: { encoding: "json", value: json(value) },
        };
    if (!retained && kind !== "observation")
      this.store.setMetadata(key, JSON.stringify(request));
    if (retained) {
      // A pinned CAS successor or our unique creation name is sufficient to
      // reconcile a lost reply even after a Hive epoch change. Never overwrite later edits.
      let observed: Document | undefined;
      if (request.create) {
        const page = await this.hive().request("objects.query", {
          contractKey: "data-collector/" + kind,
          where: { op: "eq", field: "object.name", value: request.create.name },
          includeContent: true,
          limit: 2,
        });
        requireThat(
          page.items.length <= 1,
          "target_conflict",
          "Ambiguous collector operation result.",
        );
        if (page.items[0])
          observed = await this.document(kind, page.items[0].objectId, 1);
      } else {
        try {
          observed = await this.document(
            kind,
            request.objectId!,
            request.expectedRevision! + 1,
          );
        } catch (error) {
          if (
            !["not_found", "revision_not_found"].includes(
              IvyError.from(error).code,
            )
          )
            throw error;
        }
      }
      if (
        observed &&
        canonical(json(content(observed))) === canonical(request.content.value)
      )
        return observed;
      // Exact CAS/name prevents duplicate control changes if the old operation did commit.
      request.mutationId = await newOperationId(this.hive());
      this.store.setMetadata(key, JSON.stringify(request));
    }
    const written = await this.hive().request("objects.write", request);
    return this.document(kind, written.object.id, written.revision.revision);
  }
  private noSecrets(value: unknown) {
    const secrets = Object.values(this.engine.settings.secrets).filter(Boolean);
    const check = (part: unknown) => {
      if (typeof part === "string")
        requireThat(
          !secrets.some((secret) => part.includes(secret)),
          "invalid_arguments",
          "Use secret references; configured secret values cannot enter collector history.",
        );
      else if (part && typeof part === "object")
        for (const item of Object.values(part)) check(item);
    };
    check(value);
  }
  private async assignment(jobId: string) {
    const found = (await this.all("assignment")).filter(
      (d) => assignmentDocument.parse(content(d)).jobId === jobId,
    );
    requireThat(
      found.length <= 1,
      "target_conflict",
      "Collector assignment is ambiguous.",
    );
    return found[0];
  }
  private async observation(jobId: string, serviceNodeId: string) {
    return (this.observations ?? (await this.all("observation"))).find((d) => {
      const v = observationDocument.parse(content(d));
      return v.jobId === jobId && v.serviceNodeId === serviceNodeId;
    });
  }
  private async find(id: string): Promise<Document | undefined> {
    const local = this.store.get(id, true);
    if (local?.definition)
      return this.document("definition", local.definition.jobId);
    const assignments = await this.all("assignment");
    const jobs = await this.all("definition");
    return jobs.find((d) => {
      const value = definitionDocument.parse(content(d));
      return (
        value.task.id === id &&
        (value.homeServiceNodeId === this.serviceNodeId ||
          assignments.some((a) => {
            const v = assignmentDocument.parse(content(a));
            return (
              v.jobId === d.object.id && v.serviceNodeId === this.serviceNodeId
            );
          }))
      );
    });
  }
  private async task(document: Document): Promise<Task> {
    const definition = definitionDocument.parse(content(document));
    const source = await this.document(
      "script",
      definition.script.objectId,
      definition.script.revision,
    );
    return taskSchema.parse({
      ...definition.task,
      enabled: false,
      script: scriptDocument.parse(content(source)).script,
    });
  }
  private async save(
    task: Task,
    expectedRevision: number,
    operationId: string,
    jobId?: string,
    scriptRef?: Pin,
  ) {
    this.noSecrets(task);
    if (
      this.store.metadata("definition-write:" + operationId + ":definition")
    ) {
      const saved = await this.write(
        "definition",
        {},
        operationId,
        "definition",
      );
      return {
        jobId: saved.object.id,
        revision: saved.revision.revision,
        scriptRef: definitionDocument.parse(content(saved)).script,
        task,
        assignment: await this.assignment(saved.object.id),
      };
    }
    let previous = jobId
      ? await this.document("definition", jobId)
      : await this.find(task.id);
    if (!jobId && expectedRevision === 0 && previous) {
      const assigned = await this.assignment(previous.object.id);
      if (assigned && assignmentDocument.parse(content(assigned)).deleted)
        previous = undefined;
    }
    requireThat(
      (previous?.revision.revision ?? 0) === expectedRevision,
      "revision_conflict",
      "Reload the central definition before saving.",
    );
    const prior = previous
      ? definitionDocument.parse(content(previous))
      : undefined;
    requireThat(
      !prior || prior.task.id === task.id,
      "target_conflict",
      "A job's task identifier cannot change.",
    );
    let source: Document;
    if (scriptRef) {
      source = await this.document(
        "script",
        scriptRef.objectId,
        scriptRef.revision,
      );
      requireThat(
        scriptDocument.parse(content(source)).script === task.script,
        "revision_conflict",
        "The supplied script must match its exact revision.",
      );
    } else if (prior) {
      source = await this.document(
        "script",
        prior.script.objectId,
        prior.script.revision,
      );
      if (scriptDocument.parse(content(source)).script !== task.script)
        source = await this.write(
          "script",
          { schemaVersion: 1, script: task.script },
          operationId,
          "script",
          source,
        );
    } else
      source = await this.write(
        "script",
        { schemaVersion: 1, script: task.script },
        operationId,
        "script",
      );
    const { script: _script, enabled: _enabled, ...configuration } = task;
    const saved = await this.write(
      "definition",
      {
        schemaVersion: 1,
        homeServiceNodeId: prior?.homeServiceNodeId ?? this.serviceNodeId,
        task: configuration,
        script: pin(source),
      },
      operationId,
      "definition",
      previous,
      { script: pin(source) },
    );
    return {
      jobId: saved.object.id,
      revision: saved.revision.revision,
      scriptRef: pin(source),
      task,
      assignment: await this.assignment(saved.object.id),
    };
  }
  private async activate(
    jobId: string,
    definitionRevision: number,
    expectedRevision: number,
    enabled: boolean,
    operationId: string,
    target?: string,
  ) {
    if (
      this.store.metadata("definition-write:" + operationId + ":assignment")
    ) {
      const saved = await this.write(
        "assignment",
        {},
        operationId,
        "assignment",
      );
      const desired = assignmentDocument.parse(content(saved));
      // Do not reapply an older selection after a newer assignment has superseded it.
      const current = await this.assignment(jobId);
      if (
        desired.serviceNodeId === this.serviceNodeId &&
        current?.revision.revision === saved.revision.revision
      )
        await this.accept(saved);
      return {
        jobId,
        revision: definitionRevision,
        assignment: pin(saved),
        desired,
        adoptionError: this.errors.get(jobId) ?? null,
      };
    }
    const definition = await this.document(
      "definition",
      jobId,
      definitionRevision,
    );
    const task = await this.task(definition);
    const previous = await this.assignment(jobId),
      before = previous ? assignmentDocument.parse(content(previous)) : null;
    requireThat(
      (previous?.revision.revision ?? 0) === expectedRevision,
      "revision_conflict",
      "Reload the current assignment before activating.",
    );
    const serviceNodeId = target ?? before?.serviceNodeId ?? this.serviceNodeId;
    if (before && before.serviceNodeId !== serviceNodeId) {
      const observed = await this.observation(jobId, before.serviceNodeId);
      const status = observed
        ? observationDocument.parse(content(observed))
        : null;
      requireThat(
        !before.enabled &&
          status?.assignmentRevision === previous!.revision.revision &&
          status.quiesced,
        "outcome_unknown",
        "The previous instance must acknowledge this disabled assignment and verified worker termination before reassignment.",
      );
      const prepare = async (
        node: string,
        action: "freezeHandoff" | "prepareHandoff",
      ) => {
        if (node === this.serviceNodeId)
          return action === "freezeHandoff"
            ? this.freeze(jobId, expectedRevision)
            : this.prepare(jobId, expectedRevision);
        const id = await this.mutation(operationId, action);
        return serviceTools(this.hive(), node, [
          { namespace: "data-collector", interfaceVersion: "1.0.0" },
        ]).call(
          "data-collector.update",
          { action, operationId: id, jobId, expectedRevision },
          id,
        );
      };
      await prepare(before.serviceNodeId, "freezeHandoff");
      await prepare(serviceNodeId, "prepareHandoff");
    }
    const local = this.store.get(task.id, true);
    const release = await this.write(
      "assignment",
      {
        schemaVersion: 1,
        jobId,
        definitionRevision,
        serviceNodeId,
        enabled,
        deleted: false,
        rootObjectId:
          before?.rootObjectId ?? local?.resultRootId ?? this.engine.rootId,
        resultObjectId: before?.resultObjectId ?? local?.resultObjectId ?? null,
      },
      operationId,
      "assignment",
      previous,
      { definition: { objectId: jobId, revision: definitionRevision } },
    );
    if (serviceNodeId === this.serviceNodeId) await this.accept(release);
    return {
      jobId,
      revision: definitionRevision,
      assignment: pin(release),
      desired: content(release),
      adoptionError: this.errors.get(jobId) ?? null,
    };
  }
  private unknown(row: RecordTask) {
    return (
      !!row.lastRunId &&
      this.store.run(row.lastRunId)?.error === "outcome_unknown"
    );
  }
  private async accept(document: Document) {
    const desired = assignmentDocument.parse(content(document));
    try {
      await this.adopt(document, false);
    } catch (error) {
      this.errors.set(desired.jobId, IvyError.from(error).code);
    }
  }
  private async handoff(jobId: string, revision: number) {
    const assignment = await this.assignment(jobId);
    requireThat(
      assignment && assignment.revision.revision === revision,
      "revision_conflict",
      "Reload the handoff assignment.",
    );
    const desired = assignmentDocument.parse(content(assignment));
    requireThat(
      desired.serviceNodeId === this.serviceNodeId && !desired.enabled,
      "target_conflict",
      "Handoff belongs to the disabled source instance.",
    );
    const row = this.store
      .list(true)
      .find((r) => r.definition?.jobId === jobId);
    requireThat(
      row &&
        row.definition?.assignmentRevision === revision &&
        !row.task.enabled &&
        !this.store.busy(row.task.id) &&
        !this.engine.active.has(row.task.id) &&
        !this.unknown(row),
      "outcome_unknown",
      "Handoff requires verified quiescence and a drained publication queue.",
    );
    return { assignment, row };
  }
  private async freeze(jobId: string, revision: number) {
    await this.handoff(jobId, revision);
    this.store.setMetadata("handoff-lock:" + jobId, String(revision));
    return { frozen: jobId, revision };
  }
  private async prepare(jobId: string, revision: number) {
    const assignment = await this.assignment(jobId);
    requireThat(
      assignment && assignment.revision.revision === revision,
      "revision_conflict",
      "Reload the handoff assignment.",
    );
    const desired = assignmentDocument.parse(content(assignment));
    requireThat(
      desired.serviceNodeId !== this.serviceNodeId && !desired.enabled,
      "target_conflict",
      "Prepare handoff on its new target.",
    );
    const payload = (await serviceTools(this.hive(), desired.serviceNodeId, [
      { namespace: "data-collector", interfaceVersion: "1.0.0" },
    ]).call("data-collector.read", {
      view: "handoff",
      jobId,
      expectedRevision: revision,
    })) as {
      taskId: string;
      state: Wire.Json;
      nextAt: number;
      authenticationFingerprint: string | null;
    };
    const local = this.store.get(payload.taskId, true);
    requireThat(
      !local ||
        local.deleted ||
        (local.definition?.jobId === jobId && !local.task.enabled),
      "target_conflict",
      "Another job owns the handoff task ID.",
    );
    requireThat(
      !this.store.executing(payload.taskId) &&
        !this.engine.active.has(payload.taskId),
      "task_busy",
      "The target has an active worker.",
    );
    requireThat(
      this.engine.runner.authenticationFingerprint(payload.taskId) ===
        payload.authenticationFingerprint,
      "authentication_required",
      "Private authentication caches must be securely prepared on the target; collector handoff never transfers secret values.",
    );
    this.store.setMetadata(
      "handoff-state:" + jobId,
      JSON.stringify({
        ...payload,
        assignmentId: assignment.object.id,
        assignmentRevision: revision + 1,
      }),
    );
    this.store.setMetadata("handoff-consumed:" + jobId, "false");
    return { prepared: jobId };
  }
  private async adopt(document: Document, confirmed: boolean) {
    const desired = assignmentDocument.parse(content(document));
    requireThat(
      desired.serviceNodeId === this.serviceNodeId,
      "target_conflict",
      "Only the assigned instance can adopt an execution definition.",
    );
    const definition = await this.document(
      "definition",
      desired.jobId,
      desired.definitionRevision,
    );
    const task = await this.task(definition);
    task.enabled = desired.enabled && !desired.deleted;
    const row = this.store.get(task.id, true);
    requireThat(
      !row?.definition || row.definition.jobId === desired.jobId || row.deleted,
      "target_conflict",
      "Another job already owns this local task ID.",
    );
    if (!task.enabled && row?.task.enabled) {
      this.engine.update({
        action: "disable",
        id: task.id,
        expectedRevision: row.revision,
        operationId:
          "assignment-stop:" +
          document.object.id +
          ":" +
          document.revision.revision,
      });
    }
    if (this.store.executing(task.id) || this.engine.active.has(task.id))
      return;
    requireThat(
      !row || !this.unknown(row),
      "outcome_unknown",
      "Verify the interrupted worker before accepting another execution definition.",
    );
    if (!confirmed && !row?.definition) {
      const observed = await this.observation(
        desired.jobId,
        this.serviceNodeId,
      );
      requireThat(
        !observed || !observationDocument.parse(content(observed)).everAccepted,
        "recovery_required",
        "The accepted cache was lost. Verify old workers and explicitly recover before resuming.",
      );
    }
    if (task.enabled) {
      for (const name of [
        ...task.secretNames,
        ...(task.inputSecretName ? [task.inputSecretName] : []),
      ])
        requireThat(
          this.engine.settings.secrets[name] !== undefined,
          "secret_missing",
          "An assigned secret is unavailable on this instance.",
        );
      requireThat(
        task.memoryMb <= this.engine.settings.maximumMemoryMb &&
          task.timeoutSeconds <= this.engine.settings.maximumTimeoutSeconds,
        "limit_exceeded",
        "Execution requirements exceed this instance's limits.",
      );
    }
    requireThat(
      row || this.store.list().length < this.engine.settings.maximumTasks,
      "limit_exceeded",
      "Maximum task count reached on the assigned instance.",
    );
    const current = this.store.get(task.id, true) ?? row;
    const staged = this.store.metadata("handoff-state:" + desired.jobId);
    const handoff =
      staged &&
      this.store.metadata("handoff-consumed:" + desired.jobId) !== "true"
        ? (JSON.parse(staged) as {
            assignmentId: string;
            assignmentRevision: number;
            state: unknown;
            nextAt: number;
          })
        : null;
    const transfer =
      handoff?.assignmentId === document.object.id &&
      handoff.assignmentRevision === document.revision.revision
        ? handoff
        : null;
    const changed =
      !current || canonical(json(current.task)) !== canonical(json(task));
    this.store.transaction(() => {
      this.store.save({
        task: desired.deleted
          ? {
              ...task,
              enabled: false,
              script: "",
              config: {},
              dependencies: {},
              secretNames: [],
              inputSecretName: null,
            }
          : task,
        revision: (current?.revision ?? 0) + (changed ? 1 : 0),
        state: desired.deleted
          ? null
          : transfer
            ? transfer.state
            : (current?.state ?? null),
        nextAt: transfer
          ? transfer.nextAt
          : current && !changed
            ? current.nextAt
            : Date.now() + task.intervalSeconds * 1000,
        resultObjectId:
          desired.resultObjectId ?? current?.resultObjectId ?? null,
        resultRootId: desired.rootObjectId,
        lastRunId: current?.lastRunId ?? null,
        retentionWarning: current?.retentionWarning ?? null,
        deleted: desired.deleted,
        definition: {
          jobId: desired.jobId,
          revision: desired.definitionRevision,
          assignmentId: document.object.id,
          assignmentRevision: document.revision.revision,
        },
        adoptionError: null,
      });
      if (transfer)
        this.store.setMetadata("handoff-consumed:" + desired.jobId, "true");
      const frozen = Number(
        this.store.metadata("handoff-lock:" + desired.jobId) ?? 0,
      );
      if (document.revision.revision > frozen)
        this.store.setMetadata("handoff-lock:" + desired.jobId, "0");
    });
    if (desired.deleted) this.engine.runner.clearAuthentication(task.id);
    this.errors.delete(desired.jobId);
  }
  private async report(document: Document) {
    const desired = assignmentDocument.parse(content(document));
    const row = this.store
      .list(true)
      .find((r) => r.definition?.jobId === desired.jobId);
    const accepted =
      row?.definition?.jobId === desired.jobId &&
      row.definition.assignmentRevision === document.revision.revision;
    const previous = await this.observation(desired.jobId, this.serviceNodeId);
    const value = {
      schemaVersion: 1,
      jobId: desired.jobId,
      serviceNodeId: this.serviceNodeId,
      assignmentRevision: document.revision.revision,
      definitionRevision:
        row?.definition?.revision ?? desired.definitionRevision,
      accepted,
      everAccepted:
        accepted ||
        (!!previous &&
          observationDocument.parse(content(previous)).everAccepted),
      quiesced:
        !!row &&
        !row.task.enabled &&
        !this.store.busy(row.task.id) &&
        !this.engine.active.has(row.task.id) &&
        !this.unknown(row),
      error: this.errors.get(desired.jobId) ?? null,
      observedAt: new Date().toISOString(),
    };
    const identity = canonical(json({ ...value, observedAt: null }));
    if (
      previous &&
      this.lastObserved.get(desired.jobId) === identity &&
      Date.now() -
        Date.parse(observationDocument.parse(content(previous)).observedAt) <
        30000
    )
      return;
    const saved = await this.write(
      "observation",
      value,
      "observation:" + randomUUID(),
      "status",
      previous,
    );
    if (this.observations)
      this.observations = [
        ...this.observations.filter((d) => d.object.id !== saved.object.id),
        saved,
      ];
    this.lastObserved.set(desired.jobId, identity);
  }
  async sync() {
    return this.serial(async () => {
      await this.catalog();
      const assignments = await this.all("assignment");
      this.observations = await this.all("observation");
      try {
        for (const document of assignments) {
          const value = assignmentDocument.parse(content(document));
          if (value.serviceNodeId !== this.serviceNodeId) continue;
          try {
            let row = this.store
              .list(true)
              .find((r) => r.definition?.jobId === value.jobId);
            if (
              !row ||
              row.definition?.assignmentRevision !==
                document.revision.revision ||
              this.errors.has(value.jobId)
            ) {
              await this.accept(document);
              row = this.store
                .list(true)
                .find((r) => r.definition?.jobId === value.jobId);
            }
            if (!value.resultObjectId && row?.resultObjectId) {
              await this.write(
                "assignment",
                { ...value, resultObjectId: row.resultObjectId },
                "result-binding:" +
                  document.object.id +
                  ":" +
                  row.resultObjectId,
                "binding",
                document,
                document.revision.references,
              );
            }
          } catch (error) {
            this.errors.set(value.jobId, IvyError.from(error).code);
          }
          await this.report(document);
        }
        for (const row of this.store.list()) {
          if (!row.definition) continue;
          const assignment = assignments.find(
            (a) => a.object.id === row.definition!.assignmentId,
          );
          if (
            assignment &&
            assignmentDocument.parse(content(assignment)).serviceNodeId !==
              this.serviceNodeId &&
            row.task.enabled
          ) {
            this.engine.update({
              action: "disable",
              id: row.task.id,
              expectedRevision: row.revision,
              operationId:
                "unassigned:" +
                assignment.object.id +
                ":" +
                assignment.revision.revision,
            });
          }
        }
      } finally {
        this.observations = null;
      }
    });
  }
  async read(args: unknown): Promise<unknown> {
    const input = readSchema.parse(args);
    if (input.view === "handoff") {
      const { row } = await this.handoff(input.jobId, input.expectedRevision);
      requireThat(
        this.store.metadata("handoff-lock:" + input.jobId) ===
          String(input.expectedRevision),
        "task_busy",
        "Freeze manual starts before handoff.",
      );
      this.noSecrets(row.state);
      return {
        taskId: row.task.id,
        state: json(row.state),
        nextAt: row.nextAt,
        authenticationFingerprint: this.engine.runner.authenticationFingerprint(
          row.task.id,
        ),
      };
    }
    if (input.view === "script")
      return this.document("script", input.objectId, input.revision);
    if (input.view === "definitionHistory")
      return this.hive().request("objects.history", {
        objectId: input.jobId,
        ...(input.cursor ? { cursor: input.cursor } : {}),
        limit: 50,
      });
    if (input.view === "definition") {
      const definition = await this.document(
        "definition",
        input.jobId,
        input.revision,
      );
      const assignment = await this.assignment(input.jobId);
      return {
        jobId: input.jobId,
        revision: definition.revision.revision,
        task: await this.task(definition),
        assignment,
        observation: assignment
          ? await this.observation(
              input.jobId,
              assignmentDocument.parse(content(assignment)).serviceNodeId,
            )
          : null,
      };
    }
    if (input.view === "definitions")
      return {
        items: await this.all("definition"),
        assignments: await this.all("assignment"),
        observations: await this.all("observation"),
      };
    if (input.view === "task" || input.view === "tasks") {
      const definitions = await this.all("definition"),
        assignments = await this.all("assignment");
      const runtime = (await this.engine.read({ view: "tasks" })) as {
        items: { task: { id: string }; [key: string]: unknown }[];
      };
      const items = [];
      for (const document of definitions) {
        const value = definitionDocument.parse(content(document)),
          assignment = assignments.find(
            (d) =>
              assignmentDocument.parse(content(d)).jobId === document.object.id,
          );
        const release = assignment
          ? assignmentDocument.parse(content(assignment))
          : null;
        if (
          (release?.serviceNodeId ?? value.homeServiceNodeId) !==
            this.serviceNodeId ||
          release?.deleted
        )
          continue;
        const local = runtime.items.find((r) => r.task.id === value.task.id);
        const adopted = this.store.get(value.task.id)?.definition;
        items.push({
          ...local,
          task: {
            ...value.task,
            enabled: local
              ? this.store.get(value.task.id)!.task.enabled
              : false,
            ...(input.view === "task"
              ? { script: (await this.task(document)).script }
              : {}),
          },
          jobId: document.object.id,
          revision: document.revision.revision,
          assignmentRevision: assignment?.revision.revision ?? 0,
          desiredRevision: release?.definitionRevision ?? null,
          adoptedRevision: adopted?.revision ?? null,
          desiredEnabled: release?.enabled ?? false,
          adoptionError: this.errors.get(document.object.id) ?? null,
          pendingPublications: this.store
            .runs("publishing")
            .filter((r) => r.taskId === value.task.id).length,
          status: local?.status ?? "draft",
          error: local?.error ?? null,
          lastRunAt: local?.lastRunAt ?? null,
          resultObjectId:
            local?.resultObjectId ?? release?.resultObjectId ?? null,
          retentionWarning: local?.retentionWarning ?? null,
        });
      }
      // Explicitly unimported tasks stay visible until their one-time controlled import.
      for (const local of runtime.items)
        if (
          !this.store.get(local.task.id)?.definition &&
          !items.some((r) => r.task.id === local.task.id)
        )
          items.push(local);
      if (input.view === "tasks")
        return {
          ...runtime,
          items,
          pendingPublications: this.engine.pendingPublications(),
        };
      const item = items.find((r) => r.task.id === input.id);
      requireThat(item, "not_found", "Collector task not found.");
      if (!("jobId" in item)) return this.engine.read(input);
      return item;
    }
    if (
      (input.view === "result" || input.view === "history") &&
      !this.store.get(input.id) &&
      (await this.find(input.id))
    )
      return null;
    return this.engine.read(input);
  }
  async update(args: unknown, context?: InvocationContext): Promise<unknown> {
    const input = updateSchema.parse(args);
    requireThat(
      !context?.operationId || context.operationId === input.operationId,
      "operation_conflict",
      "Routed and inner operation IDs must match.",
    );
    if (input.action === "save") this.noSecrets(input.task);
    if (input.action === "saveScript") this.noSecrets(input.script);
    if (input.action === "run") {
      const row = this.store.get(input.id);
      requireThat(
        !row?.definition ||
          !Number(
            this.store.metadata("handoff-lock:" + row.definition.jobId) ?? 0,
          ),
        "task_busy",
        "This task is frozen for handoff; activate a new assignment before manual starts.",
      );
      return this.engine.update(input, context);
    }
    return this.serial(async () => {
      const key = canonical(
        json({ caller: context?.callerPrincipalId ?? "local", input }),
      );
      if (this.store.operation(input.operationId) !== null)
        return this.store.mutate(input.operationId, key, () => null);
      const retained = this.store.metadata(
        "definition-request:" + input.operationId,
      );
      requireThat(
        !retained || retained === key,
        "operation_conflict",
        "An operation identity cannot change its definition request.",
      );
      if (!retained)
        this.store.setMetadata("definition-request:" + input.operationId, key);
      if (!retained)
        this.store.setMetadata(
          "definition-expiry:" + input.operationId,
          String(Date.now() + 7 * 86400000),
        );
      let result: unknown;
      if (input.action === "freezeHandoff")
        result = await this.freeze(input.jobId, input.expectedRevision);
      else if (input.action === "prepareHandoff")
        result = await this.prepare(input.jobId, input.expectedRevision);
      else if (input.action === "save")
        result = await this.save(
          input.task,
          input.expectedRevision,
          input.operationId,
          input.jobId,
          input.scriptRef,
        );
      else if (input.action === "saveScript") {
        this.noSecrets(input.script);
        if (
          this.store.metadata(
            "definition-write:" + input.operationId + ":script",
          )
        )
          result = await this.write("script", {}, input.operationId, "script");
        else {
          const prior = input.objectId
            ? await this.document("script", input.objectId)
            : undefined;
          requireThat(
            (prior?.revision.revision ?? 0) === input.expectedRevision,
            "revision_conflict",
            "Reload the script revision.",
          );
          result = await this.write(
            "script",
            { schemaVersion: 1, script: input.script },
            input.operationId,
            "script",
            prior,
          );
        }
      } else if (input.action === "activate")
        result = await this.activate(
          input.jobId,
          input.definitionRevision,
          input.expectedRevision,
          input.enabled,
          input.operationId,
          input.serviceNodeId,
        );
      else if (input.action === "import") {
        const imported = [];
        for (const row of this.store.list()) {
          if (row.definition) {
            imported.push(row.definition);
            continue;
          }
          const existing = await this.find(row.task.id);
          const saved = existing
            ? {
                jobId: existing.object.id,
                revision: existing.revision.revision,
              }
            : await this.save(
                row.task,
                0,
                input.operationId + ":" + row.task.id,
              );
          let assignment = await this.assignment(saved.jobId);
          if (!assignment) {
            await this.activate(
              saved.jobId,
              saved.revision,
              0,
              row.task.enabled,
              input.operationId + ":" + row.task.id + ":activate",
            );
            assignment = await this.assignment(saved.jobId);
          }
          await this.adopt(assignment!, true);
          imported.push({ id: row.task.id, jobId: saved.jobId });
        }
        result = { imported };
      } else if (input.action === "recover") {
        const assignment = await this.assignment(input.jobId);
        requireThat(
          assignment && assignment.revision.revision === input.expectedRevision,
          "revision_conflict",
          "Reload the assignment before recovery.",
        );
        requireThat(
          assignmentDocument.parse(content(assignment)).serviceNodeId ===
            this.serviceNodeId,
          "target_conflict",
          "Recover on the assigned instance.",
        );
        await this.adopt(assignment, true);
        result = { recovered: input.jobId };
      } else {
        const definition = await this.find(input.id);
        requireThat(
          definition && definition.revision.revision === input.expectedRevision,
          "revision_conflict",
          "Reload the central definition revision.",
        );
        const assignment = await this.assignment(definition.object.id);
        requireThat(
          !assignment ||
            assignmentDocument.parse(content(assignment)).serviceNodeId ===
              this.serviceNodeId,
          "target_conflict",
          "Change the task on its assigned instance.",
        );
        if (input.action === "delete")
          requireThat(
            !this.store.busy(input.id),
            "task_busy",
            "Publish pending work and stop workers before deleting a task.",
          );
        if (input.action === "disable" && input.confirmedStoppedRunId) {
          const row = this.store.get(input.id)!;
          this.engine.update({
            ...input,
            operationId: "definition-confirm:" + input.operationId,
            expectedRevision: row.revision,
          });
        }
        result = await this.activate(
          definition.object.id,
          input.action === "enable"
            ? definition.revision.revision
            : assignment
              ? assignmentDocument.parse(content(assignment)).definitionRevision
              : definition.revision.revision,
          assignment?.revision.revision ?? 0,
          input.action === "enable",
          input.operationId,
        );
        if (input.action === "delete") {
          requireThat(
            !this.store.executing(input.id) &&
              !this.engine.active.has(input.id),
            "task_busy",
            "Wait for worker termination before deleting.",
          );
          const released = await this.assignment(definition.object.id);
          const deleted = await this.write(
            "assignment",
            { ...assignmentDocument.parse(content(released!)), deleted: true },
            input.operationId,
            "delete",
            released,
            released!.revision.references,
          );
          await this.adopt(deleted, true);
        }
      }
      return this.store.mutate(input.operationId, key, () => result);
    });
  }
}
