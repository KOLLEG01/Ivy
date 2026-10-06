import {
  newOperationId,
  serviceTools,
} from "../../../packages/sdk/src/client.js";
import type { Operation, Wire } from "../../../packages/sdk/src/client.js";
import type { Task } from "../../../services/data-collector/src/schema.js";
import { client } from "./runtime";

export type { Task };
export const tr = (de: string, en: string) =>
  navigator.language.toLowerCase().startsWith("de") ? de : en;

export interface TaskRow {
  task: Pick<Task, "id" | "name" | "enabled">;
  revision: number;
  status: string;
  error: string | null;
  lastRunAt: string | null;
  retentionWarning: string | null;
  resultObjectId: string | null;
}
export interface TasksView {
  items: TaskRow[];
  limits: {
    maximumMemoryMb: number;
    maximumTimeoutSeconds: number;
    maximumTasks: number;
  } | null;
}

const tools = (node: string) =>
  serviceTools(client, node, [
    { namespace: "data-collector", interfaceVersion: "1.0.0" },
  ]);
export const read = async <T>(node: string, input: unknown): Promise<T> =>
  (await tools(node).call("data-collector.read", input as Wire.Json)) as T;
export async function update(
  node: string,
  action: string,
  extra: Record<string, unknown>,
) {
  const operationId = await newOperationId(client);
  await tools(node).call(
    "data-collector.update",
    { action, ...extra, operationId } as Wire.Json,
    operationId,
  );
}
export async function collectorNodes(): Promise<Operation.ServiceNode[]> {
  const found: Operation.ServiceNode[] = [];
  let next: string | null = null;
  do {
    const page: Operation.ServiceNodesListResult = await client.request(
      "serviceNodes.list",
      {
        serviceName: "data-collector",
        limit: 200,
        ...(next ? { cursor: next } : {}),
      },
    );
    found.push(...page.items);
    next = page.nextCursor;
  } while (next);
  return found;
}

/** Actionable guidance for the error codes a task script may report; raw messages stay private. */
export const taskError = (code: string) =>
  (
    ({
      authentication_required: tr(
        "Zugangsdaten oder Token erneuern.",
        "Renew the credentials or token.",
      ),
      interaction_required: tr(
        "Anbieter-Verifizierung gemäß Einrichtungsanleitung abschließen.",
        "Complete provider verification as described in the setup guide.",
      ),
      configuration_invalid: tr(
        "Task-Einstellungen und zugewiesene Secrets prüfen.",
        "Check task settings and assigned secrets.",
      ),
      provider_unavailable: tr(
        "Anbieter nicht verfügbar oder Antwort nicht unterstützt.",
        "Provider unavailable or response unsupported.",
      ),
    }) as Record<string, string>
  )[code] ?? code;

export const statusLabel = (status: string) =>
  (
    ({
      idle: tr("Noch nicht gelaufen", "Not run yet"),
      queued: tr("Wartet", "Queued"),
      running: tr("Läuft", "Running"),
      publishing: tr("Speichert", "Saving"),
      succeeded: tr("Erfolgreich", "Succeeded"),
      failed: tr("Fehlgeschlagen", "Failed"),
      cancelled: tr("Abgebrochen", "Cancelled"),
      interrupted: tr("Unterbrochen", "Interrupted"),
    }) as Record<string, string>
  )[status] ?? status;
export const statusTone = (status: string) =>
  status === "succeeded"
    ? "good"
    : status === "failed"
      ? "bad"
      : status === "cancelled" || status === "interrupted"
        ? "warning"
        : "neutral";
export const working = (status: string) =>
  ["queued", "running", "publishing"].includes(status);

export const intervalLabel = (seconds: number) =>
  !seconds
    ? tr("Nur manuell oder per Eingang", "Manual or input only")
    : seconds % 3600 === 0
      ? tr(`Alle ${seconds / 3600} h`, `Every ${seconds / 3600} h`)
      : seconds % 60 === 0
        ? tr(`Alle ${seconds / 60} min`, `Every ${seconds / 60} min`)
        : tr(`Alle ${seconds} s`, `Every ${seconds} s`);

export const newTask = (): Task => ({
  id: "",
  name: "",
  enabled: false,
  intervalSeconds: 300,
  timeoutSeconds: 60,
  memoryMb: 128,
  dependencies: {},
  config: {},
  secretNames: [],
  retention: { maximumCount: 1000, maximumAgeDays: 7, maximumBytes: 104857600 },
  script:
    "export default async ({ config, secrets, state, input, signal }) => {\n  return { data: { collectedAt: new Date().toISOString() } };\n};",
});
/** Task IDs are stable slugs; a new task derives one from its name until the user edits it. */
export const slugOf = (name: string) =>
  name
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^[-_]+|[-_]+$/g, "")
    .slice(0, 64);
