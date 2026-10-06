import { readFileSync } from "node:fs";
import { IvyError } from "../packages/contracts/src/errors.js";

export const hiveMcpInstructions = readFileSync(
  new URL("./hive-mcp-instructions.md", import.meta.url),
  "utf8",
).trim();

export const hiveMcpAgentSummary = readFileSync(
  new URL("./hive-mcp-agent-summary.md", import.meta.url),
  "utf8",
).trim();

export const hiveMcpExamples = readFileSync(
  new URL("./hive-mcp-examples.md", import.meta.url),
  "utf8",
).trim();

const capabilitiesHeading = "\n\nServices registered to this Hive provide the following capabilities:\n";
const toolWorkflowsHeading = "\n\nTool workflows:\n";

/** Project only the live service hints into the always-loaded agent override. */
export function compactHiveMcpInstructions(instructions: string): string {
  const start = instructions.indexOf(capabilitiesHeading);
  if (start < 0) return hiveMcpAgentSummary;
  const end = instructions.indexOf(toolWorkflowsHeading, start);
  return hiveMcpAgentSummary + instructions.slice(start, end < 0 ? undefined : end);
}

export function composeHiveMcpInstructions(
  hints: Iterable<{ serviceName: string; discoveryHint: string | undefined }>,
  guides: Iterable<string> = [],
): string {
  const entries = new Map<string, Set<string>>();
  for (const { serviceName, discoveryHint } of hints) {
    const hint = discoveryHint?.trim();
    if (!hint) continue;
    const values = entries.get(serviceName) ?? new Set<string>();
    values.add(hint);
    entries.set(serviceName, values);
  }
  const lines = [...entries]
    .sort(([left], [right]) => left.localeCompare(right, "en"))
    .flatMap(([service, values]) =>
      [...values]
        .sort((left, right) => left.localeCompare(right, "en"))
        .map((hint) => `- ${service}: ${hint}`),
    );
  const workflows = [...new Set(guides)].sort((left, right) => left.localeCompare(right, "en"));
  const instructions = hiveMcpInstructions
    + (lines.length ? capabilitiesHeading + lines.join("\n") : "")
    + (workflows.length ? toolWorkflowsHeading + workflows.map((guide) => `- ${guide}`).join("\n") : "");
  if (Buffer.byteLength(instructions) > 32768)
    throw new IvyError(
      "limit_exceeded",
      "Composed Hive instructions exceed 32 KiB.",
    );
  return instructions;
}
