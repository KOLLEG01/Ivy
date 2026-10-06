import { requireThat } from "../../../../packages/sdk/src/node.js";
import type { Chat, Wire } from "../../../../packages/sdk/src/node.js";
import type { ChatBridge } from "../bridge.js";
import { resolveChatPlan } from "../native-plan.js";
import { WhatsAppJournal } from "./journal.js";
import type { Inbox } from "./journal.js";
import { WhatsAppNative } from "./native.js";
import { scopedOperationId } from "../../../../packages/sdk/src/client.js";
import { translator } from "./messages.js";
import type { Translator } from "./messages.js";
export type Preferences = NonNullable<Chat.Payload["turnOptions"]> & {
  commentary: boolean;
};
export const parseCommand = (
  text: string,
): { name: string; args: string } | null => {
  const match = /^\/([a-z]+)(?:\s+([\s\S]*))?$/i.exec(text.trim());
  return match
    ? { name: match[1]!.toLowerCase(), args: match[2]?.trim() ?? "" }
    : null;
};
const efforts = new Set([
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
]);
const defaults = new Set(["default", "reset", "auto"]);
export class WhatsAppCommands {
  constructor(
    readonly bridge: ChatBridge,
    readonly journal: WhatsAppJournal,
    readonly native: WhatsAppNative,
    readonly desktop: (action: string, key: string) => Promise<string>,
    readonly t: Translator = translator(),
  ) {}
  preferences(): Preferences {
    return (
      this.journal.get<Preferences>("preferences") ?? { commentary: false }
    );
  }
  turnOptions(): NonNullable<Chat.Payload["turnOptions"]> {
    const { commentary: _commentary, ...saved } = this.preferences();
    return {
      ...saved,
      model: saved.model ?? "gpt-5.6-terra",
      effort: saved.effort ?? "medium",
    };
  }
  async run(row: Inbox, text: string): Promise<string | null> {
    const command = parseCommand(text);
    requireThat(command, "invalid_arguments", this.t("command.invalid"));
    const { name, args } = command,
      normalized = args.toLowerCase();
    if (["help", "start"].includes(name)) return this.t("help");
    if (name === "new") {
      requireThat(!args, "invalid_arguments", this.t("new.usage"));
      const prior = row.request
        ? (JSON.parse(row.request) as Chat.CreateMainRequest)
        : null;
      let request = prior;
      if (!request) {
        const main = await this.bridge.main.find();
        request = {
          action: "createMain",
          operationId: await scopedOperationId(this.bridge.main.store.client, [
            "whatsapp-new",
            row.id,
          ]),
          expectedBridge: this.bridge.main.admission.expected(row.sender),
          expectedMainRevision: main?.pin.revision ?? null,
          reason: this.t("new.reason"),
        };
        this.journal.saveRequest(row.id, request);
      }
      const result = await this.bridge.main.create(row.sender, request);
      if (result.phase === "failed")
        return this.t("new.failed", { code: result.error?.code ?? "unknown" });
      if (
        result.phase !== "succeeded" ||
        result.outcome?.action !== "createMain"
      )
        return null;
      this.journal.set(
        "listener-new:" + result.outcome.binding.object.objectId,
        true,
      );
      this.native.invalidate();
      return this.t("new.success", {
        id: result.outcome.binding.data.primary.nativeId,
      });
    }
    if (name === "steer") {
      requireThat(
        args.length > 0 && [...args].length <= 65536,
        "invalid_arguments",
        this.t("steer.usage"),
      );
      const result = await this.native.command(
        row.id,
        "turn/steer",
        async () => {
          const main = await this.bridge.main.find();
          requireThat(
            main?.value.binding,
            "chat_main_missing",
            this.t("steer.first"),
          );
          const binding = await this.bridge.main.store.read(
            "chat-bridge/binding",
            main.value.binding,
          );
          const page = await this.native.read("thread/turns/list", {
            threadId: binding.value.primary.nativeId,
            limit: 1,
            itemsView: "notLoaded",
            sortDirection: "desc",
          });
          const turn = (page["data"] as Record<string, Wire.Json>[])[0];
          requireThat(
            turn?.["status"] === "inProgress",
            "chat_no_active_turn",
            this.t("steer.inactive"),
          );
          this.journal.set("steer-binding:" + row.id, binding.pin.objectId);
          return {
            threadId: binding.value.primary.nativeId,
            expectedTurnId: turn["id"]!,
            input: [{ type: "text", text: args, text_elements: [] }],
            clientUserMessageId: "wa-steer-" + row.id,
          };
        },
        async () => {
          const main = await this.bridge.main.find();
          requireThat(
            main?.value.binding?.objectId ===
              this.journal.get<string>("steer-binding:" + row.id),
            "chat_binding_mismatch",
            this.t("steer.changed"),
          );
        },
      );
      return result === null ? null : this.t("steer.success");
    }
    if (name === "desktop") return this.desktop(args, row.id);
    if (
      ["status", "task"].includes(name) ||
      (["model", "reasoning", "fast", "commentary"].includes(name) && !args)
    ) {
      requireThat(
        !args || name !== "task",
        "invalid_arguments",
        this.t("task.status_only"),
      );
      const main = await this.bridge.main.find(),
        binding = main?.value.binding
          ? await this.bridge.main.store.read(
              "chat-bridge/binding",
              main.value.binding,
            )
          : null;
      const plan = await resolveChatPlan(
          this.bridge.main.store,
          this.bridge.main.admission.definition.nativePlan,
        ),
        p = { ...plan.turnStart, ...this.preferences(), ...this.turnOptions() };
      const mode = plan.turnStart["collaborationMode"] as {
        settings?: { model?: string; reasoning_effort?: string };
      } | null;
      return this.t("status", {
        main: binding?.value.primary.nativeId ?? this.t("status.pending"),
        queue: main?.value.queue.length ?? 0,
        model:
          p.model ??
          mode?.settings?.model ??
          (typeof plan.threadStart["model"] === "string"
            ? plan.threadStart["model"]
            : this.t("status.default")),
        reasoning:
          p.effort ??
          mode?.settings?.reasoning_effort ??
          this.t("status.default"),
        fast: this.t(
          p.serviceTier === "fast" ? "status.enabled" : "status.disabled",
        ),
        commentary: this.t(p.commentary ? "status.enabled" : "status.disabled"),
      });
    }
    if (["commentary", "fast"].includes(name)) {
      const on = ["on", "an", "true", "1", "yes", "ja"].includes(normalized),
        off = ["off", "aus", "false", "0", "no", "nein"].includes(normalized);
      requireThat(
        on || off,
        "invalid_arguments",
        this.t("toggle.usage", { name }),
      );
      const p = this.preferences();
      if (name === "commentary") p.commentary = on;
      else p.serviceTier = on ? "fast" : null;
      this.journal.set("preferences", p);
      return this.t("toggle.result", {
        setting: this.t(name === "fast" ? "toggle.fast" : "toggle.commentary"),
        state: this.t(on ? "status.enabled" : "status.disabled"),
      });
    }
    if (name === "reasoning") {
      requireThat(
        defaults.has(normalized) || efforts.has(normalized),
        "invalid_arguments",
        this.t("reasoning.invalid"),
      );
      this.journal.set("preferences", {
        ...this.preferences(),
        effort: defaults.has(normalized) ? null : normalized,
      });
      return this.t("reasoning.result", { effort: normalized });
    }
    if (name === "models" || name === "model") {
      const result = await this.native.command(
        row.id,
        "model/list",
        async () => ({ limit: 100, includeHidden: false }),
      );
      if (result === null) return null;
      const models = (
        result as {
          data: Array<{
            id: string;
            model: string;
            displayName: string;
            supportedReasoningEfforts?: Array<{ reasoningEffort: string }>;
          }>;
        }
      ).data;
      requireThat(
        Array.isArray(models),
        "native_read_failed",
        this.t("models.missing"),
      );
      if (name === "models")
        return models
          .map((m) => `${m.model || m.id} — ${m.displayName}`)
          .join("\n");
      const [model, effort, ...extra] = normalized.split(/\s+/);
      requireThat(
        model && !extra.length && (!effort || efforts.has(effort)),
        "invalid_arguments",
        this.t("model.usage"),
      );
      const selected = models.find(
        (m) => m.model.toLowerCase() === model || m.id.toLowerCase() === model,
      );
      requireThat(
        defaults.has(model) || selected,
        "invalid_arguments",
        this.t("model.unavailable"),
      );
      requireThat(
        !effort ||
          !selected?.supportedReasoningEfforts ||
          selected.supportedReasoningEfforts.some(
            (e) => e.reasoningEffort === effort,
          ),
        "invalid_arguments",
        this.t("model.reasoning"),
      );
      this.journal.set("preferences", {
        ...this.preferences(),
        model: defaults.has(model) ? null : selected!.model || selected!.id,
        ...(effort ? { effort } : {}),
      });
      return this.t("model.result", { model });
    }
    if (["threads", "tasks"].includes(name)) {
      const limit = args ? Number(args) : 10;
      requireThat(
        Number.isInteger(limit) && limit >= 1 && limit <= 25,
        "invalid_arguments",
        this.t("threads.usage"),
      );
      const result = await this.native.command(
        row.id,
        "thread/list",
        async () => ({ limit, sortKey: "updated_at" }),
      );
      if (result === null) return null;
      const rows = (
        result as {
          data: Array<{ id: string; name?: string; preview?: string }>;
        }
      ).data;
      return (
        this.t("threads.header") +
        "\n" +
        rows.map((t) => `${t.id} — ${t.name ?? t.preview ?? ""}`).join("\n")
      );
    }
    return this.t("command.unknown", { help: this.t("help") });
  }
}
