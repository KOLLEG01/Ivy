import { readFileSync, writeFileSync } from "node:fs";

const paths = [
  "brave-container/SKILL.md",
  "brave-container/agents/openai.yaml",
  "brave-container/scripts/brave-container.ps1",
];
const value = {
  instructions: {
    schemaVersion: 1,
    hostId: null,
    enabled: true,
    includeLocal: true,
    text: readFileSync("instructions/default-agent-instructions.md", "utf8"),
  },
  mcp: {
    schemaVersion: 1,
    hostId: null,
    enabled: true,
    servers: [
      {
        name: "ivy",
        url: "https://ivy.invalid/mcp",
        enabled: true,
        authentication: "agent-manager",
        startupTimeoutSeconds: 15,
        toolTimeoutSeconds: 45,
      },
      {
        name: "ivy_dev",
        url: "https://ivy.invalid/mcp-dev",
        enabled: true,
        authentication: "agent-manager",
        startupTimeoutSeconds: 15,
        toolTimeoutSeconds: 45,
      },
    ],
  },
  skills: {
    schemaVersion: 1,
    hostId: null,
    enabled: true,
    files: paths.map((path) => ({
      path,
      content: readFileSync("instructions/skills/" + path, "utf8"),
    })),
  },
};
const path = "services/agent-manager/src/environment-defaults.json",
  content = JSON.stringify(value, null, 2) + "\n";
if (process.argv.includes("--check")) {
  if (readFileSync(path, "utf8") !== content)
    throw new Error(`Stale generated defaults: ${path}`);
} else writeFileSync(path, content);
