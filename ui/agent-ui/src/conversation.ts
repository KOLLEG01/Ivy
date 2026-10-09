import type { Agent } from "../../../packages/sdk/src/client.js";
import { list, record, text } from "../../../packages/ui-client/src/native";
import { messageImages } from "../../../packages/ui-client/src/message-images";
import type { MessageImage } from "../../../packages/ui-client/src/message-images";

export interface ConversationItem {
  kind: "message" | "activity" | "hidden";
  label: string;
  text: string;
  markdown: boolean;
  images?: MessageImage[];
}

const compact = (value: string, maximum = 260): string => {
  const normalized = value.replace(/\s+/g, " ").trim();
  return normalized.length <= maximum
    ? normalized
    : normalized.slice(0, maximum - 1).trimEnd() + "…";
};

/** Keep the conversation at Codex Desktop's user-facing detail level. Native command output,
 * tool payloads and reasoning content remain available from the explicit turn download. */
export function conversationItem(item: unknown): ConversationItem {
  const value = record(item),
    type = text(value.type);
  if (type === "userMessage")
    return {
      kind: "message",
      label: "You",
      markdown: false,
      images: messageImages(value.content),
      text: list(value.content)
        .map((part) => text(record(part).text))
        .filter(Boolean)
        .join("\n"),
    };
  if (type === "agentMessage")
    return {
      kind: "message",
      label: "Assistant",
      text: text(value.text),
      markdown: true,
    };
  if (type === "plan")
    return {
      kind: "message",
      label: "Plan",
      text: text(value.text),
      markdown: true,
    };
  if (type === "reasoning" || type === "functionCallOutput")
    return { kind: "hidden", label: "", text: "", markdown: false };
  if (type === "commandExecution")
    return {
      kind: "activity",
      label: "Command",
      text: compact(text(value.command, "Command executed")),
      markdown: false,
    };
  if (type === "fileChange") {
    const changes = list(value.changes),
      paths = changes
        .map((change) => text(record(change).path))
        .filter(Boolean);
    const shown = paths.slice(0, 3).join(", "),
      more = Math.max(0, paths.length - 3);
    return {
      kind: "activity",
      label: "Files changed",
      text: compact(shown + (more ? ` and ${more} more` : ""), 320),
      markdown: false,
    };
  }
  if (type === "mcpToolCall") {
    const context = record(value.appContext),
      app = text(context.appName) || text(value.server),
      tool = text(context.actionName) || text(value.tool);
    return {
      kind: "activity",
      label: "Tool",
      text: compact([app, tool].filter(Boolean).join(" · ") || "Tool used"),
      markdown: false,
    };
  }
  if (type === "dynamicToolCall")
    return {
      kind: "activity",
      label: "Tool",
      text: compact(
        [text(value.namespace), text(value.tool)].filter(Boolean).join(" · ") ||
          "Tool used",
      ),
      markdown: false,
    };
  if (type === "webSearch") {
    const action = record(value.action),
      target =
        text(action.query) ||
        list(action.queries)
          .map((entry) => text(entry))
          .filter(Boolean)
          .join(", ") ||
        text(action.url);
    return {
      kind: "activity",
      label: "Search",
      text: compact(target || "Web search"),
      markdown: false,
    };
  }
  if (type === "collabAgentToolCall")
    return {
      kind: "activity",
      label: "Agent",
      text: compact(text(value.tool, "Agent activity")),
      markdown: false,
    };
  return {
    kind: "activity",
    label: itemLabel(value),
    text: "",
    markdown: false,
  };
}

export function liveMessages(
  activity: Agent.Notification[],
  savedIds: Set<string>,
  turnId = "",
): Array<{ id: string; text: string }> {
  const messages = new Map<string, { id: string; text: string }>();
  for (const event of activity) {
    const params = record(event.params), item = record(params.item),
      completed = event.method === 'item/completed' && ['agentMessage', 'plan'].includes(text(item.type)) && typeof item.text === 'string',
      id = text(completed ? item.id : params.itemId);
    if ((!completed && event.method !== 'item/agentMessage/delta') || !id || savedIds.has(id) || (turnId && params.turnId !== turnId)) continue;
    const value = messages.get(id) ?? { id, text: "" };
    if (completed) value.text = text(item.text);
    else value.text += text(params.delta);
    messages.set(id, value);
  }
  return [...messages.values()];
}
export function itemLabel(item: unknown): string {
  const value = record(item),
    type = text(value.type);
  return (
    (
      {
        userMessage: "You",
        agentMessage: "Assistant",
        commandExecution: "Command",
        fileChange: "Files changed",
        mcpToolCall: "Tool",
        reasoning: "Thinking",
        webSearch: "Search",
        plan: "Plan",
      } as Record<string, string>
    )[type] ?? "Activity"
  );
}

export interface ConversationStep {
  id: string;
  presentation: ConversationItem;
  running: boolean;
}

/** Tool steps that the native owner reported live but that saved output does not contain yet. */
export function liveSteps(
  activity: Agent.Notification[],
  savedIds: Set<string>,
  turnId: string,
): ConversationStep[] {
  const steps = new Map<string, ConversationStep>();
  for (const event of activity) {
    if (event.method !== "item/started" && event.method !== "item/completed")
      continue;
    const params = record(event.params),
      item = record(params.item),
      id = text(item.id);
    if (!id || savedIds.has(id) || (turnId && params.turnId !== turnId))
      continue;
    const presentation = conversationItem(item);
    if (presentation.kind !== "activity") continue;
    steps.set(id, {
      id,
      presentation,
      running: event.method === "item/started",
    });
  }
  return [...steps.values()];
}

const summaryParts: Record<string, (count: number) => string> = {
  Command: (count) => (count === 1 ? "ran a command" : `ran ${count} commands`),
  "Files changed": () => "edited files",
  Tool: (count) => (count === 1 ? "used a tool" : `used ${count} tools`),
  Search: () => "searched the web",
  Agent: () => "worked with agents",
};

/** One line for consecutive tool steps, in the order their kinds first occurred. */
export function activitySummary(steps: ConversationStep[]): string {
  const counts = new Map<string, number>();
  for (const step of steps) {
    const label = Object.hasOwn(summaryParts, step.presentation.label)
      ? step.presentation.label
      : "Activity";
    counts.set(label, (counts.get(label) ?? 0) + 1);
  }
  const text = [...counts]
    .map(([label, count]) =>
      label === "Activity"
        ? count === 1
          ? "1 other step"
          : `${count} other steps`
        : summaryParts[label]!(count),
    )
    .join(", ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}
