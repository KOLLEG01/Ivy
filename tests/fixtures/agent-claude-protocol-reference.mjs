// Public adapter test reference, including current empty-turn/EOF handling.
// MIT: services/agent-manager/claude/LICENSE.adapter.
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __export = (target, all) => {
  for (var name2 in all)
    __defProp(target, name2, { get: all[name2], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __reExport = (target, mod, secondTarget) => (__copyProps(target, mod, "default"), secondTarget && __copyProps(secondTarget, mod, "default"));

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/server.mts
import { execFile as execFile2, spawn as spawn3 } from "node:child_process";
import { existsSync as existsSync6, readFileSync as readFileSync6, watch, writeFileSync as writeFileSync3 } from "node:fs";

// fixture-os:fixture-os
var fixture_os_exports = {};
__export(fixture_os_exports, {
  homedir: () => homedir
});
__reExport(fixture_os_exports, node_os_star);
import * as node_os_star from "node:os";
var homedir = () => globalThis.__ivyClaudeFixture.home;

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/server.mts
import { join as join8 } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify as promisify2 } from "node:util";

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/claude-capabilities.mts
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
function listClaudeSkills(params) {
  const roots2 = cwdsFromParams(params);
  const userSkills = readSkillDir(join(homedir(), ".claude", "skills"), "user");
  return roots2.map((cwd) => {
    const repo = readSkillDir(join(cwd, ".claude", "skills"), "repo");
    return {
      cwd,
      skills: [...userSkills.skills, ...repo.skills],
      errors: [...userSkills.errors, ...repo.errors]
    };
  });
}
function readSkillDir(dir, scope) {
  const skills = [];
  const errors = [];
  if (!existsSync(dir)) return { skills, errors };
  let entries = [];
  try {
    entries = readdirSync(dir);
  } catch (error) {
    errors.push({ path: dir, message: messageOf(error) });
    return { skills, errors };
  }
  for (const entry of entries) {
    const skillPath = join(dir, entry);
    const manifest = join(skillPath, "SKILL.md");
    try {
      if (!statSync(skillPath).isDirectory() || !existsSync(manifest)) continue;
      const frontmatter = parseFrontmatter(readFileSync(manifest, "utf8"));
      const name2 = frontmatter.name || entry;
      skills.push({
        name: name2,
        description: frontmatter.description || "",
        ...frontmatter.description ? { shortDescription: frontmatter.description } : {},
        path: skillPath,
        scope,
        enabled: true
      });
    } catch (error) {
      errors.push({ path: manifest, message: messageOf(error) });
    }
  }
  return { skills, errors };
}
var HOOK_EVENT_MAP = {
  PreToolUse: "preToolUse",
  PostToolUse: "postToolUse",
  PermissionRequest: "permissionRequest",
  PreCompact: "preCompact",
  PostCompact: "postCompact",
  SessionStart: "sessionStart",
  UserPromptSubmit: "userPromptSubmit",
  Stop: "stop"
};
function listClaudeHooks(params) {
  const roots2 = cwdsFromParams(params);
  const userSources = [
    { path: join(homedir(), ".claude", "settings.json"), source: "user" }
  ];
  return roots2.map((cwd) => {
    const sources = [
      ...userSources,
      { path: join(cwd, ".claude", "settings.json"), source: "project" },
      { path: join(cwd, ".claude", "settings.local.json"), source: "project" }
    ];
    const hooks = [];
    const errors = [];
    let order = 0;
    for (const { path, source } of sources) {
      if (!existsSync(path)) continue;
      let parsed;
      try {
        parsed = JSON.parse(readFileSync(path, "utf8"));
      } catch (error) {
        errors.push({ path, message: messageOf(error) });
        continue;
      }
      const hookConfig = asRecord(parsed.hooks);
      for (const [claudeEvent, eventName] of Object.entries(HOOK_EVENT_MAP)) {
        const matchers = hookConfig[claudeEvent];
        if (!Array.isArray(matchers)) continue;
        for (const rawMatcher of matchers) {
          const matcherEntry = asRecord(rawMatcher);
          const matcher = typeof matcherEntry.matcher === "string" ? matcherEntry.matcher : null;
          const handlers = Array.isArray(matcherEntry.hooks) ? matcherEntry.hooks : [];
          for (const rawHandler of handlers) {
            const handler = asRecord(rawHandler);
            const command = typeof handler.command === "string" ? handler.command : null;
            const handlerType = handler.type === "prompt" ? "prompt" : handler.type === "agent" ? "agent" : "command";
            hooks.push({
              key: `${source}:${eventName}:${order}`,
              eventName,
              handlerType,
              matcher,
              command,
              timeoutSec: typeof handler.timeout === "number" ? handler.timeout : 60,
              statusMessage: null,
              sourcePath: path,
              source,
              pluginId: null,
              displayOrder: order,
              enabled: true,
              isManaged: false,
              currentHash: createHash("sha256").update(`${eventName}:${matcher ?? ""}:${command ?? ""}`).digest("hex"),
              trustStatus: "trusted"
            });
            order += 1;
          }
        }
      }
    }
    return { cwd, hooks, warnings: [], errors };
  });
}
function cwdsFromParams(params) {
  const cwds = Array.isArray(params.cwds) ? params.cwds.map(String).filter(Boolean) : [];
  return cwds.length > 0 ? cwds : [process.cwd()];
}
function parseFrontmatter(text) {
  if (!text.startsWith("---")) return {};
  const end = text.indexOf("\n---", 3);
  if (end < 0) return {};
  const body = text.slice(3, end);
  const out = {};
  for (const line of body.split("\n")) {
    const match = /^\s*(name|description)\s*:\s*(.+?)\s*$/.exec(line);
    if (!match) continue;
    const value = (match[2] ?? "").replace(/^["']|["']$/g, "");
    if (match[1] === "name") out.name = value;
    else out.description = value;
  }
  return out;
}
function asRecord(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}
function messageOf(error) {
  return error instanceof Error ? error.message : String(error);
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/model-discovery.mts
function runtimeModelList(models, configuredModel, configuredEffort) {
  const first = models[0];
  if (!first) throw new Error("Claude CLI returned no available models");
  const rows = /* @__PURE__ */ new Map();
  const idOf = (model) => model.resolvedModel && !/\[.+\]$/.test(model.value) ? model.resolvedModel : model.value;
  const defaultId = idOf(
    models.find((model) => model.value === configuredModel || idOf(model) === configuredModel) ?? models.find((model) => model.value === "default") ?? first
  );
  for (const model of models) {
    const id = idOf(model);
    if (!rows.has(id) || model.value !== "default") rows.set(id, model);
  }
  return {
    data: [...rows].map(([id, model]) => {
      const efforts = model.supportsEffort === false ? [] : model.supportedEffortLevels ?? [];
      const version = /^claude-[a-z][a-z-]*-(\d+(?:-\d+)*)$/.exec((model.resolvedModel ?? id).replace(/-\d{8}$/, ""))?.[1]?.replaceAll("-", ".");
      return {
        id,
        model: id,
        upgrade: null,
        upgradeInfo: null,
        availabilityNux: null,
        displayName: version && !model.displayName.includes(version) ? `${model.displayName} ${version}` : model.displayName,
        description: model.description,
        hidden: false,
        supportedReasoningEfforts: efforts.map((reasoningEffort) => ({
          reasoningEffort,
          description: reasoningEffort
        })),
        defaultReasoningEffort: efforts.includes(configuredEffort) ? configuredEffort : efforts[0] ?? "none",
        inputModalities: ["text", "image"],
        supportsPersonality: false,
        additionalSpeedTiers: [],
        isDefault: id === defaultId
      };
    }),
    nextCursor: null
  };
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/projects.mts
import { existsSync as existsSync3, readFileSync as readFileSync3, renameSync as renameSync2, writeFileSync } from "node:fs";
import { isAbsolute, join as join3, normalize } from "node:path";

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/util.mts
import { createHash as createHash2 } from "node:crypto";
import {
  appendFileSync,
  existsSync as existsSync2,
  mkdirSync,
  readdirSync as readdirSync2,
  readFileSync as readFileSync2,
  renameSync,
  statSync as statSync2,
  unlinkSync
} from "node:fs";
import { dirname, join as join2, resolve } from "node:path";
function nowSeconds() {
  return Math.floor(Date.now() / 1e3);
}
function nowMillis() {
  return Date.now();
}
function newId() {
  return globalThis.__ivyClaudeFixture.nextId();
}
function codexHome() {
  return resolve(process.env.CODEX_HOME || join2(homedir(), ".codex"));
}
function adapterHome() {
  return resolve(process.env.CLAUDE_CODEX_HOME || join2(codexHome(), "claude-codex-adapter"));
}
function ensureParent(path) {
  mkdirSync(dirname(path), { recursive: true, mode: 448 });
}
function debugLog(event, data = {}) {
  if (process.env.CLAUDE_CODEX_DEBUG_LOG === "0" || process.env.CLAUDE_CODEX_DEBUG_LOG === "false")
    return;
  const path = resolve(process.env.CLAUDE_CODEX_DEBUG_LOG || join2(adapterHome(), "debug.jsonl"));
  try {
    ensureParent(path);
    rotateLogIfNeeded(path);
    appendFileSync(
      path,
      JSON.stringify({
        ts: (/* @__PURE__ */ new Date()).toISOString(),
        pid: process.pid,
        event,
        ...redact(data)
      }) + "\n"
    );
  } catch {
  }
}
function rotateLogIfNeeded(path) {
  const maxBytes = numericEnv("CLAUDE_CODEX_DEBUG_LOG_MAX_BYTES", 50 * 1024 * 1024);
  if (maxBytes <= 0) return;
  const keep = Math.max(1, numericEnv("CLAUDE_CODEX_DEBUG_LOG_KEEP", 3));
  let size = 0;
  try {
    size = statSync2(path).size;
  } catch {
    return;
  }
  if (size < maxBytes) return;
  const oldest = `${path}.${keep}`;
  try {
    unlinkSync(oldest);
  } catch {
  }
  for (let i = keep - 1; i >= 1; i -= 1) {
    try {
      renameSync(`${path}.${i}`, `${path}.${i + 1}`);
    } catch {
    }
  }
  try {
    renameSync(path, `${path}.1`);
  } catch {
  }
}
function numericEnv(name2, fallback) {
  const raw = process.env[name2];
  if (!raw) return fallback;
  const parsed = Number(raw);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}
function platformFamily() {
  return (0, fixture_os_exports.platform)() === "win32" ? "windows" : "unix";
}
function platformOs() {
  switch ((0, fixture_os_exports.platform)()) {
    case "darwin":
      return "macos";
    case "win32":
      return "windows";
    default:
      return (0, fixture_os_exports.platform)();
  }
}
var DEFAULT_CODEX_COMPAT_VERSION = "0.142.3";
function codexCompatVersion() {
  return process.env.CLAUDE_CODEX_COMPAT_VERSION || process.env.CODEX_SHIM_COMPAT_VERSION || DEFAULT_CODEX_COMPAT_VERSION;
}
var DEFAULT_VERSION_SUFFIX = "claude-codex";
function codexVersionSuffix() {
  return process.env.CLAUDE_CODEX_VERSION_SUFFIX ?? DEFAULT_VERSION_SUFFIX;
}
function codexCliVersion() {
  const suffix = codexVersionSuffix();
  return `codex-cli ${codexCompatVersion()}${suffix ? ` (${suffix})` : ""}`;
}
function codexUserAgent(clientName, clientVersion) {
  const name2 = clientName.trim() || "codex-app";
  const version = clientVersion.trim() || "unknown";
  const cpu = process.arch === "x64" ? "x86_64" : process.arch === "arm64" ? "aarch64" : process.arch;
  const originator = codexVersionSuffix() || "unknown";
  return `${name2}/${codexCompatVersion()} (${platformOs()}; ${cpu}) ${originator} (${name2}; ${version})`;
}
function textFromInput(input) {
  if (!Array.isArray(input)) return "";
  return input.map((item) => {
    if (!item || typeof item !== "object") return "";
    const value = item;
    if (value.type === "text") return value.text ?? "";
    if (value.type === "mention") return `@${value.path ?? value.name ?? ""}`;
    if (value.type === "localImage") return `[local image: ${value.path ?? ""}]`;
    if (value.type === "image") return `[image]`;
    if (value.type === "skill") return `/${value.name ?? "skill"}`;
    return "";
  }).filter(Boolean).join("\n");
}
function extractImageInputs(input) {
  if (!Array.isArray(input)) return { textPrompt: "", images: [] };
  const texts = [];
  const images = [];
  for (const raw of input) {
    if (!raw || typeof raw !== "object") continue;
    const item = raw;
    if (item.type === "text" && typeof item.text === "string") texts.push(item.text);
    else if (item.type === "mention") texts.push(`@${item.path ?? item.name ?? ""}`);
    else if (item.type === "skill") texts.push(`/${item.name ?? "skill"}`);
    else if (item.type === "localImage" && typeof item.path === "string") {
      const img = readLocalImage(item.path);
      if (img) images.push(img);
      else texts.push(`[local image unavailable: ${item.path}]`);
    } else if (item.type === "image" && typeof item.url === "string") {
      const img = parseImageUrl(item.url);
      if (img) images.push(img);
      else texts.push("[image]");
    }
  }
  return { textPrompt: texts.filter(Boolean).join("\n"), images };
}
function readLocalImage(path) {
  try {
    const stat = statSync2(path);
    if (stat.size > 10 * 1024 * 1024) return null;
    const buf = readFileSync2(path);
    return {
      kind: "base64",
      mediaType: sniffImageMediaType(path),
      data: buf.toString("base64"),
      displayPath: path
    };
  } catch {
    return null;
  }
}
function parseImageUrl(url) {
  if (url.startsWith("data:")) {
    const match = url.match(/^data:([^;,]+)(?:;([^,]+))?,(.*)$/);
    if (!match) return null;
    const mediaType = match[1] || "application/octet-stream";
    const isBase64 = (match[2] ?? "").includes("base64");
    const payload = match[3] ?? "";
    return {
      kind: "base64",
      mediaType,
      data: isBase64 ? payload : Buffer.from(decodeURIComponent(payload), "utf8").toString("base64"),
      displayPath: url.slice(0, 64) + (url.length > 64 ? "..." : "")
    };
  }
  if (url.startsWith("http://") || url.startsWith("https://") || url.startsWith("file://")) {
    return { kind: "url", mediaType: "image/*", data: url, displayPath: url };
  }
  return null;
}
function sniffImageMediaType(path) {
  const lower = path.toLowerCase();
  if (lower.endsWith(".png")) return "image/png";
  if (lower.endsWith(".jpg") || lower.endsWith(".jpeg")) return "image/jpeg";
  if (lower.endsWith(".webp")) return "image/webp";
  if (lower.endsWith(".gif")) return "image/gif";
  if (lower.endsWith(".bmp")) return "image/bmp";
  return "image/png";
}
function jsonClone(value) {
  return JSON.parse(JSON.stringify(value));
}
function stableHash(value) {
  return createHash2("sha256").update(value).digest("hex");
}
function modelDisplayName(model) {
  if (model === "default") return "Claude Default";
  if (model === "sonnet") return "Claude Sonnet";
  if (model === "opus") return "Claude Opus";
  if (model === "fable") return "Claude Fable";
  if (model === "haiku") return "Claude Haiku";
  if (model === "sonnet-1m") return "Claude Sonnet 1M";
  if (model === "opus-plan") return "Claude Opus Plan / Sonnet Execute";
  return model;
}
function modelDescription(model) {
  if (model === "opus-plan" || model === "claude-opus-plan" || model === "opusplan") {
    return "Uses Opus in plan mode and Sonnet for execution; selecting this model does not enable plan mode.";
  }
  return "Claude Code runtime model";
}
function scanDiscoveredRouterModels() {
  const models = /* @__PURE__ */ new Set();
  models.add("sonnet");
  models.add("opus");
  models.add("fable");
  models.add("haiku");
  models.add("sonnet-1m");
  models.add("opus-plan");
  const cacheBase = join2(homedir(), ".cache");
  if (existsSync2(cacheBase)) {
    try {
      for (const sub of readdirSync2(cacheBase)) {
        const routerDir = join2(cacheBase, sub, "claude-router");
        if (existsSync2(routerDir)) {
          for (const item of readdirSync2(routerDir)) {
            if (!item.startsWith(".")) models.add(item);
          }
        }
      }
    } catch {
    }
  }
  const extraRoots = (process.env.CLAUDE_CONFIG_DIRS || process.env.CLAUDE_ROUTER_DIR || "").split(/[,:]/).map((p) => p.trim()).filter(Boolean);
  for (const extra of extraRoots) {
    if (existsSync2(extra)) {
      try {
        for (const item of readdirSync2(extra)) {
          if (!item.startsWith(".")) models.add(item);
        }
      } catch {
      }
    }
  }
  return models;
}
var cachedDiscoveredModels = null;
function getDiscoveredRouterModelsCached() {
  const now = Date.now();
  if (cachedDiscoveredModels && cachedDiscoveredModels.expiresAt > now) {
    return cachedDiscoveredModels.set;
  }
  const set = scanDiscoveredRouterModels();
  cachedDiscoveredModels = { expiresAt: now + 15e3, set };
  return set;
}
var cachedClaudeModelOptions = null;
var cachedCodexProxyModelOptions = null;
function claudeModelOptions() {
  const configured = process.env.CLAUDE_CODEX_MODELS;
  if (configured) {
    try {
      const parsed = JSON.parse(configured);
      if (Array.isArray(parsed)) {
        return parsed.map((entry) => {
          if (typeof entry === "string") return modelOption(entry);
          if (entry && typeof entry === "object") {
            const record = entry;
            const id = typeof record.id === "string" ? record.id : String(record.model ?? "sonnet");
            return {
              id,
              sdkModel: typeof record.sdkModel === "string" ? record.sdkModel : resolveClaudeModel(id),
              displayName: typeof record.displayName === "string" ? record.displayName : modelDisplayName(id),
              description: typeof record.description === "string" ? record.description : modelDescription(id),
              isDefault: record.isDefault === true
            };
          }
          return modelOption("sonnet");
        });
      }
    } catch {
    }
    return configured.split(",").map((part) => modelOption(part.trim())).filter((entry) => entry.id.length > 0);
  }
  const now = Date.now();
  if (cachedClaudeModelOptions && cachedClaudeModelOptions.expiresAt > now) {
    return cachedClaudeModelOptions.data;
  }
  const models = /* @__PURE__ */ new Set();
  models.add("sonnet");
  models.add("opus");
  models.add("fable");
  models.add("haiku");
  models.add("sonnet-1m");
  models.add("opus-plan");
  const cacheBase = join2(homedir(), ".cache");
  if (existsSync2(cacheBase)) {
    try {
      for (const sub of readdirSync2(cacheBase)) {
        const routerDir = join2(cacheBase, sub, "claude-router");
        if (existsSync2(routerDir)) {
          for (const item of readdirSync2(routerDir)) {
            if (!item.startsWith(".")) models.add(item);
          }
        }
      }
    } catch {
    }
  }
  const extraRoots = (process.env.CLAUDE_CONFIG_DIRS || process.env.CLAUDE_ROUTER_DIR || "").split(/[,:]/).map((p) => p.trim()).filter(Boolean);
  for (const extra of extraRoots) {
    if (existsSync2(extra)) {
      try {
        for (const item of readdirSync2(extra)) {
          if (!item.startsWith(".")) models.add(item);
        }
      } catch {
      }
    }
  }
  const result = Array.from(models).map((m, i) => modelOption(m, i === 0));
  cachedClaudeModelOptions = { expiresAt: now + 15e3, data: result };
  return result;
}
function resolveClaudeModel(model, purpose = "normal") {
  const raw = (model ?? "").trim();
  if (purpose === "summary") {
    const summaryModel = process.env.CLAUDE_CODEX_SUMMARY_MODEL || process.env.CLAUDE_CODEX_TITLE_MODEL || "haiku";
    if (isCodexOpenAiModel(raw) || !raw) return summaryModel;
  }
  if (!raw || raw === "default" || raw === "claude-default")
    return process.env.CLAUDE_CODEX_DEFAULT_MODEL || null;
  if (raw === "claude-code" || raw === "custom")
    return process.env.CLAUDE_CODEX_DEFAULT_MODEL || null;
  const aliases = {
    "claude-sonnet": "sonnet",
    "claude-opus": "opus",
    "claude-fable": "fable",
    "claude-haiku": "haiku",
    "sonnet-1m": "sonnet[1m]",
    "claude-sonnet-1m": "sonnet[1m]",
    "opus-plan": "opusplan",
    "claude-opus-plan": "opusplan"
  };
  const envAliases = parseJsonObject(process.env.CLAUDE_CODEX_MODEL_ALIASES);
  const mapped = typeof envAliases[raw] === "string" ? envAliases[raw] : aliases[raw];
  if (mapped) return mapped;
  const configured = process.env.CLAUDE_CODEX_MODELS;
  if (configured) {
    const list = configured.split(",").map((s) => s.trim()).filter(Boolean);
    const matched = list.find((m) => m.toLowerCase() === raw.toLowerCase());
    if (matched) return matched;
  }
  const discoveredIds = getDiscoveredRouterModelsCached();
  for (const id of discoveredIds) {
    if (id.toLowerCase() === raw.toLowerCase()) return id;
  }
  if (isNativeClaudeModel(raw)) return raw;
  return process.env.CLAUDE_CODEX_DEFAULT_MODEL || null;
}
function defaultAllowedTools() {
  const raw = process.env.CLAUDE_CODEX_ALLOWED_TOOLS;
  if (raw == null || raw.trim() === "") return null;
  const value = raw.trim();
  if (value === "*" || value.toLowerCase() === "default") return null;
  return value.split(",").map((part) => part.trim()).filter(Boolean);
}
function claudeOutputFormat(outputSchema) {
  if (outputSchema == null) return null;
  if (outputSchema && typeof outputSchema === "object" && !Array.isArray(outputSchema)) {
    const record = outputSchema;
    if (record.type === "json_schema" && record.schema != null) return outputSchema;
  }
  return { type: "json_schema", schema: outputSchema };
}
function normalizeCodexReasoningEffort(value) {
  if (value === "low" || value === "medium" || value === "high" || value === "xhigh") return value;
  if (value === "max") return value;
  if (value === "minimal") return "low";
  return null;
}
function resolveClaudeEffort(value) {
  const raw = (value ?? "").trim();
  const envAliases = parseJsonObject(process.env.CLAUDE_CODEX_EFFORT_ALIASES);
  const mapped = typeof envAliases[raw] === "string" ? envAliases[raw] : raw;
  if (mapped === "low" || mapped === "medium" || mapped === "high" || mapped === "xhigh" || mapped === "max")
    return mapped;
  if (mapped === "minimal") return "low";
  return null;
}
function modelOption(id, isDefault = false, description = modelDescription(id)) {
  return {
    id,
    sdkModel: resolveClaudeModel(id),
    displayName: modelDisplayName(id),
    description,
    isDefault
  };
}
function parseJsonObject(raw) {
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}
function isNativeClaudeModel(model) {
  return !isCodexOpenAiModel(model);
}
function isCodexOpenAiModel(model) {
  return /^gpt[-_]/.test(model) || /^o[0-9]/.test(model) || model.startsWith("codex-");
}
function codexProxyModelOptions() {
  if (process.env.CLAUDE_CODEX_MOCK === "1") return [];
  if (process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY === "1") return [];
  if (!resolveCodexBinary()) return [];
  const now = Date.now();
  if (cachedCodexProxyModelOptions && cachedCodexProxyModelOptions.expiresAt > now) {
    return cachedCodexProxyModelOptions.data;
  }
  const env = process.env.CLAUDE_CODEX_CODEX_MODELS;
  let ids = [];
  if (env && env.trim()) {
    ids = env.split(",").map((s) => s.trim()).filter(Boolean);
  } else {
    const catalogCandidates = [join2(homedir(), ".codex/model_catalog.json")];
    for (const p of catalogCandidates) {
      if (existsSync2(p)) {
        try {
          const cat = JSON.parse(readFileSync2(p, "utf8"));
          if (cat && Array.isArray(cat.models)) {
            ids = cat.models.map((m) => m.slug || m.id || m.model).filter(Boolean);
            break;
          }
        } catch {
        }
      }
    }
    if (ids.length === 0) {
      ids = ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.5", "gpt-5.4", "gpt-5.4-mini"];
    }
  }
  return ids.map((id) => ({
    id,
    sdkModel: id,
    displayName: `Codex \xB7 ${id}`,
    description: "Real OpenAI Codex CLI (forwarded via `codex exec --json`).",
    isDefault: false
  }));
}
function resolveCodexBinary() {
  const explicit = process.env.CODEX_REAL;
  if (explicit && explicit.trim()) return explicit.trim();
  const knownCandidates = [
    join2(
      homedir(),
      ".local/node/lib/node_modules/@openai/codex/node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex.real"
    )
  ];
  for (const c of knownCandidates) {
    if (existsSync2(c)) return c;
  }
  const paths = (process.env.PATH ?? "").split(":").filter(Boolean);
  for (const dir of paths) {
    const candidate = `${dir}/codex`;
    try {
      const st = statSync2(candidate);
      if (!st.isFile()) continue;
      const head = readFileSync2(candidate, { encoding: "utf8" }).slice(0, 200);
      if (head.includes("CLAUDE_CODEX_ADAPTER") || head.includes("claude-codex")) continue;
      return candidate;
    } catch {
    }
  }
  return null;
}
function redact(value, depth = 0) {
  if (depth > 5) return "[truncated]";
  if (typeof value === "string")
    return value.length > 1200 ? value.slice(0, 1200) + "...[truncated]" : value;
  if (value == null || typeof value !== "object") return value;
  if (Array.isArray(value)) return value.slice(0, 40).map((entry) => redact(entry, depth + 1));
  const result = {};
  for (const [key, entry] of Object.entries(value).slice(0, 80)) {
    if (/token|secret|password|api.?key|authorization|cookie/i.test(key)) {
      result[key] = "[redacted]";
    } else {
      result[key] = redact(entry, depth + 1);
    }
  }
  return result;
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/projects.mts
function name(value) {
  if (typeof value !== "string" || !value.trim())
    throw new Error("Project name is required");
  return value.trim();
}
function roots(value) {
  if (!Array.isArray(value)) throw new Error("Project roots must be an array");
  return value.map((root) => {
    const path = root?.path;
    if (typeof path !== "string" || !isAbsolute(path))
      throw new Error("Project roots must be absolute paths");
    return { path: normalize(path) };
  });
}
function metadata(value) {
  if (value == null) return {};
  if (typeof value !== "object" || Array.isArray(value) || Object.values(value).some((entry) => typeof entry !== "string"))
    throw new Error("Project metadata must contain strings");
  return Object.fromEntries(
    Object.entries(value).sort(([a], [b]) => a.localeCompare(b))
  );
}
var ProjectStore = class {
  path = join3(adapterHome(), "projects.json");
  state;
  ephemeral = /* @__PURE__ */ new Map();
  thread;
  constructor(thread) {
    this.thread = thread;
    this.state = existsSync3(this.path) ? JSON.parse(readFileSync3(this.path, "utf8")) : { projects: [], requests: {}, threads: {} };
  }
  save(next) {
    ensureParent(this.path);
    const temporary = this.path + ".tmp";
    writeFileSync(temporary, JSON.stringify(next) + "\n", { mode: 384 });
    renameSync2(temporary, this.path);
    this.state = next;
  }
  summary(project) {
    let recencyAt = null;
    for (const [id, projectId] of Object.entries(this.state.threads)) {
      if (projectId !== project.id) continue;
      const thread = this.thread(id);
      if (thread && !thread.archived && !thread.ephemeral)
        recencyAt = Math.max(recencyAt ?? 0, thread.updatedAt);
    }
    return { ...structuredClone(project), recencyAt };
  }
  read(id) {
    const project = this.state.projects.find((entry) => entry.id === id);
    if (!project) throw new Error("Unknown project: " + String(id));
    return this.summary(project);
  }
  list(params) {
    if (params.sortDirection != null && params.sortKey == null)
      throw new Error("Project sort direction requires a sort key");
    const recency = params.sortKey === "recencyAt";
    const direction = params.sortDirection === "asc" ? 1 : params.sortDirection === "desc" ? -1 : recency ? -1 : 1;
    const projects = this.state.projects.map((entry) => this.summary(entry)).sort((a, b) => {
      if (recency && (a.recencyAt === null || b.recencyAt === null))
        return a.recencyAt === b.recencyAt ? a.position - b.position : a.recencyAt === null ? 1 : -1;
      return direction * (recency ? a.recencyAt - b.recencyAt : a.position - b.position) || a.id.localeCompare(b.id);
    });
    const offset = params.cursor == null ? 0 : Number(params.cursor);
    if (!Number.isSafeInteger(offset) || offset < 0)
      throw new Error("Invalid project cursor");
    const limit = Math.max(1, Math.min(Number(params.limit ?? 100), 200));
    return {
      data: projects.slice(offset, offset + limit),
      nextCursor: offset + limit < projects.length ? String(offset + limit) : null
    };
  }
  create(params) {
    if (typeof params.idempotencyKey !== "string" || !params.idempotencyKey)
      throw new Error("Project creation requires an idempotency key");
    const input = {
      name: name(params.name),
      roots: roots(params.roots),
      metadata: metadata(params.metadata)
    };
    const fingerprint = JSON.stringify(input);
    const previous = Object.hasOwn(this.state.requests, params.idempotencyKey) ? this.state.requests[params.idempotencyKey] : void 0;
    if (previous) {
      if (previous.fingerprint !== fingerprint)
        throw new Error("Project idempotency key belongs to another request");
      return this.read(previous.projectId);
    }
    const now = nowSeconds();
    const project = {
      ...input,
      id: newId(),
      position: Math.max(-1, ...this.state.projects.map((entry) => entry.position)) + 1,
      createdAt: now,
      updatedAt: now
    };
    const next = structuredClone(this.state);
    next.projects.push(project);
    next.requests = {
      ...next.requests,
      [params.idempotencyKey]: { fingerprint, projectId: project.id }
    };
    this.save(next);
    return this.summary(project);
  }
  update(params) {
    const project = this.read(params.projectId);
    const next = structuredClone(this.state);
    const target = next.projects.find((entry) => entry.id === project.id);
    if (params.name != null) target.name = name(params.name);
    if (params.roots != null) target.roots = roots(params.roots);
    if (params.metadata != null) target.metadata = metadata(params.metadata);
    target.updatedAt = nowSeconds();
    this.save(next);
    return this.summary(target);
  }
  delete(id) {
    const project = this.read(id);
    const next = structuredClone(this.state);
    next.projects = next.projects.filter((entry) => entry.id !== project.id);
    for (const [threadId, projectId] of Object.entries(next.threads))
      if (projectId === project.id) delete next.threads[threadId];
    this.save(next);
    for (const [threadId, projectId] of this.ephemeral)
      if (projectId === project.id) this.ephemeral.delete(threadId);
  }
  bind(thread, projectId) {
    this.read(projectId);
    if (thread.ephemeral) {
      this.ephemeral.set(thread.id, projectId);
      return;
    }
    const next = structuredClone(this.state);
    next.threads[thread.id] = projectId;
    this.save(next);
  }
  projectId(thread) {
    let current = thread;
    const seen = /* @__PURE__ */ new Set();
    while (current && !seen.has(current.id)) {
      seen.add(current.id);
      const id = this.ephemeral.get(current.id) ?? this.state.threads[current.id];
      if (id) return id;
      current = current.forkedFromId ? this.thread(current.forkedFromId) : null;
    }
    return null;
  }
};

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/rate-limits.mts
import { existsSync as existsSync4, readFileSync as readFileSync4, renameSync as renameSync3, writeFileSync as writeFileSync2 } from "node:fs";
import { join as join4 } from "node:path";
var WEEK_MINUTES = 7 * 24 * 60;
var LABELS = {
  seven_day_opus: "Weekly \xB7 Opus",
  seven_day_sonnet: "Weekly \xB7 Sonnet",
  seven_day_oauth_apps: "Weekly \xB7 OAuth apps"
};
var percent = (value) => Math.min(100, Math.max(0, Math.round(value)));
var seconds = (value) => {
  if (typeof value === "number" && Number.isFinite(value)) return Math.round(value);
  if (typeof value !== "string") return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? Math.round(parsed / 1e3) : null;
};
var RateLimitStore = class {
  path = join4(adapterHome(), "rate-limits.json");
  windows = {};
  usageAt = 0;
  constructor() {
    try {
      if (existsSync4(this.path)) this.windows = JSON.parse(readFileSync4(this.path, "utf8"));
    } catch {
      this.windows = {};
    }
  }
  /** Whether the on-demand `/usage` read is due; it runs at most once per interval. */
  usageDue(now = Date.now(), intervalMs = 6e4) {
    return now - this.usageAt >= intervalMs;
  }
  /** A `rate_limit_info` from a turn; its utilization is a fraction of the window. */
  recordEvent(info, now = Date.now()) {
    const type = typeof info?.rateLimitType === "string" ? info.rateLimitType : "";
    if (!type || type === "overage" || typeof info?.utilization !== "number") return;
    this.save({
      ...this.windows,
      [type]: {
        usedPercent: percent(info.utilization * 100),
        resetsAt: seconds(info.resetsAt),
        observedAt: now,
        ...info.status === "rejected" ? { rejected: true } : {}
      }
    });
  }
  /** The `rate_limits` of the structured `/usage` data; utilization is already a percentage. */
  recordUsage(limits, now = Date.now()) {
    this.usageAt = now;
    if (!limits) return;
    const next = { ...this.windows };
    const add = (key, value) => {
      const window = value;
      if (typeof window?.utilization !== "number") return;
      next[key] = { usedPercent: percent(window.utilization), resetsAt: seconds(window.resets_at), observedAt: now };
    };
    for (const key of ["five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet", "seven_day_oauth_apps"])
      add(key, limits[key]);
    if (Array.isArray(limits.model_scoped)) {
      for (const entry of limits.model_scoped)
        if (typeof entry?.display_name === "string" && entry.display_name)
          add("model:" + entry.display_name, entry);
    }
    this.save(next);
  }
  /** The Codex response; a window whose reset has passed starts again at zero. */
  read(nowSeconds2 = Math.floor(Date.now() / 1e3)) {
    const window = (key, minutes) => {
      const value = this.windows[key];
      if (!value) return null;
      const expired = value.resetsAt !== null && value.resetsAt <= nowSeconds2;
      return { usedPercent: expired ? 0 : value.usedPercent, windowDurationMins: minutes, resetsAt: expired ? null : value.resetsAt };
    };
    const reached = Object.values(this.windows).some(
      (value) => value.rejected && (value.resetsAt === null || value.resetsAt > nowSeconds2)
    );
    const bucket = (limitId, limitName, primary, secondary = null) => ({
      limitId,
      limitName,
      primary,
      secondary,
      credits: null,
      planType: null,
      rateLimitReachedType: reached && limitId === "claude-code" ? "rate_limit_reached" : null
    });
    const rateLimits = bucket("claude-code", "Claude Code", window("five_hour", 300), window("seven_day", WEEK_MINUTES));
    const byId = { "claude-code": rateLimits };
    for (const key of Object.keys(this.windows).sort()) {
      if (key === "five_hour" || key === "seven_day") continue;
      const name2 = LABELS[key] ?? (key.startsWith("model:") ? "Weekly \xB7 " + key.slice(6) : null);
      if (name2) byId[key] = bucket(key, name2, window(key, WEEK_MINUTES));
    }
    return { rateLimits, rateLimitsByLimitId: byId };
  }
  save(next) {
    this.windows = next;
    try {
      ensureParent(this.path);
      const temporary = this.path + ".tmp";
      writeFileSync2(temporary, JSON.stringify(next) + "\n", { mode: 384 });
      renameSync3(temporary, this.path);
    } catch {
    }
  }
};

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/mcp.mts
import { spawn } from "node:child_process";
import { existsSync as existsSync5, readFileSync as readFileSync5 } from "node:fs";
function readMcpConfig() {
  const raw = process.env.CLAUDE_CODEX_MCP_SERVERS;
  if (!raw) return { sdkValue: null, listStatuses: [], startupStatuses: [] };
  try {
    const sdkValue = parseMcpValue(raw);
    const names = serverNames(sdkValue);
    return {
      sdkValue,
      listStatuses: names.map((name2) => listStatusEntry(name2)),
      // The adapter passes MCP servers straight to the Claude Agent SDK; it
      // does not run a separate startup handshake, so the optimistic terminal
      // state is "ready" rather than the invalid "pending".
      startupStatuses: names.map((name2) => ({ name: name2, status: "ready", error: null }))
    };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      sdkValue: null,
      listStatuses: [listStatusEntry("CLAUDE_CODEX_MCP_SERVERS")],
      startupStatuses: [{ name: "CLAUDE_CODEX_MCP_SERVERS", status: "failed", error: message }]
    };
  }
}
var statusCache = null;
var STATUS_CACHE_MS = 1e4;
async function listMcpServerStatuses() {
  const snapshot = readMcpConfig();
  const names = serverNames(snapshot.sdkValue);
  const key = JSON.stringify(snapshot.sdkValue ?? null);
  if (statusCache && statusCache.key === key && Date.now() - statusCache.at < STATUS_CACHE_MS) {
    return statusCache.data;
  }
  const data = await Promise.all(names.map((name2) => enumerateServerStatus(name2)));
  statusCache = { key, at: Date.now(), data };
  return data;
}
async function enumerateServerStatus(name2) {
  const base = listStatusEntry(name2);
  const [tools, resources, resourceTemplates] = await Promise.all([
    safeMcpList(name2, "tools/list", "tools"),
    safeMcpList(name2, "resources/list", "resources"),
    safeMcpList(name2, "resources/templates/list", "resourceTemplates")
  ]);
  if (Array.isArray(tools)) base.tools = toolMap(tools);
  if (Array.isArray(resources)) {
    base.resources = resources.map((raw) => asRecord2(raw)).filter((r) => typeof r.uri === "string");
  }
  if (Array.isArray(resourceTemplates)) {
    base.resourceTemplates = resourceTemplates.map((raw) => asRecord2(raw)).filter((r) => typeof r.uriTemplate === "string");
  }
  return base;
}
function toolMap(tools) {
  const entries = tools.map((raw) => asRecord2(raw)).filter((tool) => {
    return typeof tool.name === "string";
  }).map((tool) => [
    tool.name,
    {
      name: tool.name,
      ...typeof tool.title === "string" ? { title: tool.title } : {},
      ...typeof tool.description === "string" ? { description: tool.description } : {},
      inputSchema: tool.inputSchema ?? {},
      ...tool.outputSchema ? { outputSchema: tool.outputSchema } : {}
    }
  ]);
  return Object.fromEntries(entries);
}
async function safeMcpList(name2, method, key) {
  const result = await runMcpRequest(name2, method, {}).then(
    (value2) => asRecord2(value2),
    () => null
  );
  if (!result) return null;
  const value = result[key];
  return Array.isArray(value) ? value : [];
}
async function callMcpTool(serverName, toolName, args) {
  const result = await runMcpRequest(serverName, "tools/call", {
    name: toolName,
    arguments: args ?? {}
  });
  const record = asRecord2(result);
  return {
    content: Array.isArray(record.content) ? record.content : [],
    structuredContent: record.structuredContent ?? null,
    isError: Boolean(record.isError),
    _meta: record._meta ?? null
  };
}
async function readMcpResource(serverName, uri) {
  const result = await runMcpRequest(serverName, "resources/read", { uri });
  const record = asRecord2(result);
  return { contents: Array.isArray(record.contents) ? record.contents : [] };
}
async function runMcpRequest(serverName, method, params) {
  const config = getServerConfig(serverName);
  if (config.type === "http") {
    return runHttpMcpRequest(config, method, params);
  }
  if (config.type && config.type !== "stdio") {
    throw new Error(
      `direct MCP ${method} only supports stdio servers; ${serverName} is ${config.type}`
    );
  }
  const command = stringOr(config.command, "");
  if (!command) throw new Error(`MCP server ${serverName} has no command`);
  const child = spawn(command, Array.isArray(config.args) ? config.args.map(String) : [], {
    env: { ...process.env, ...stringRecord(config.env) },
    stdio: ["pipe", "pipe", "pipe"]
  });
  const pending = /* @__PURE__ */ new Map();
  let nextId = 1;
  let buffer = "";
  let stderr = "";
  let spawnError = null;
  child.on("error", (error) => {
    spawnError = error;
    for (const wait of pending.values()) wait.reject(error);
    pending.clear();
  });
  child.stdin.on("error", () => void 0);
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    stderr += String(chunk);
  });
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    buffer += String(chunk);
    let lineEnd;
    while ((lineEnd = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, lineEnd).trim();
      buffer = buffer.slice(lineEnd + 1);
      if (!line || !line.startsWith("{")) continue;
      const message = JSON.parse(line);
      const id = Number(message.id);
      const wait = pending.get(id);
      if (!wait) continue;
      pending.delete(id);
      if (message.error) wait.reject(new Error(JSON.stringify(message.error)));
      else wait.resolve(message.result);
    }
  });
  const request = (requestMethod, requestParams) => {
    if (spawnError) return Promise.reject(spawnError);
    const id = nextId++;
    child.stdin.write(
      `${JSON.stringify({ jsonrpc: "2.0", id, method: requestMethod, params: requestParams })}
`
    );
    return new Promise((resolve5, reject) => pending.set(id, { resolve: resolve5, reject }));
  };
  const timeout = setTimeout(() => {
    for (const wait of pending.values())
      wait.reject(new Error(`MCP request timed out${stderr ? `: ${stderr}` : ""}`));
    pending.clear();
    child.kill();
  }, 2e4);
  try {
    await request("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "claude-codex-adapter", version: "0.1.0" }
    });
    child.stdin.write(
      `${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized", params: {} })}
`
    );
    return await request(method, params);
  } finally {
    clearTimeout(timeout);
    child.kill();
  }
}
async function runHttpMcpRequest(config, method, params) {
  const url = stringOr(config.url, "");
  if (!url) throw new Error("HTTP MCP server has no url");
  const headers = {
    "content-type": "application/json",
    accept: "application/json",
    ...stringRecord(config.headers)
  };
  const initialize = await fetchJsonRpc(url, headers, {
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "claude-codex-adapter", version: "0.1.0" }
    }
  });
  if (asRecord2(initialize).error) throw new Error(JSON.stringify(asRecord2(initialize).error));
  const response = await fetchJsonRpc(url, headers, { jsonrpc: "2.0", id: 2, method, params });
  const record = asRecord2(response);
  if (record.error) throw new Error(JSON.stringify(record.error));
  return record.result;
}
async function fetchJsonRpc(url, headers, body) {
  const response = await fetch(url, { method: "POST", headers, body: JSON.stringify(body) });
  const text = await response.text();
  if (!response.ok) throw new Error(`HTTP MCP request failed ${response.status}: ${text}`);
  const trimmed = text.trim();
  if (trimmed.startsWith("event:") || trimmed.startsWith("data:")) {
    const dataLine = trimmed.split("\n").find((line) => line.startsWith("data:"));
    return JSON.parse((dataLine ?? "").slice("data:".length).trim());
  }
  return JSON.parse(trimmed);
}
function getServerConfig(serverName) {
  const snapshot = readMcpConfig();
  const value = snapshot.sdkValue;
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("CLAUDE_CODEX_MCP_SERVERS is not an object config");
  const config = value[serverName];
  if (!config || typeof config !== "object" || Array.isArray(config))
    throw new Error(`unknown MCP server: ${serverName}`);
  return config;
}
function parseMcpValue(raw) {
  const trimmed = raw.trim();
  if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
    return JSON.parse(trimmed);
  }
  if (existsSync5(trimmed)) {
    return JSON.parse(readFileSync5(trimmed, "utf8"));
  }
  return JSON.parse(readFileSync5(trimmed, "utf8"));
}
function serverNames(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return [];
  return Object.keys(value);
}
function listStatusEntry(name2) {
  return {
    name: name2,
    tools: {},
    resources: [],
    resourceTemplates: [],
    authStatus: "unsupported"
  };
}
function asRecord2(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}
function stringOr(value, fallback) {
  return typeof value === "string" && value.length > 0 ? value : fallback;
}
function stringRecord(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(
    Object.entries(value).map(([key, raw]) => [key, String(raw)])
  );
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/provider-loop-descriptor-types.mts
var CREDENTIAL_SOURCES = [
  "user-api-key",
  "cloud-provider",
  "organization-gateway",
  "local-cli-auth"
];
var UNSUPPORTED_CREDENTIAL_SOURCES = [
  "personal-subscription",
  "personal-session",
  "browser-cookie",
  "oauth-session",
  "cli-session-export",
  "credential-sharing",
  "private-proxy",
  "provider-bypass"
];
var DESCRIPTOR_STATUSES = ["stable", "experimental"];
var PROVIDER_FAMILIES = ["anthropic", "openai"];
var GATEWAY_POLICIES = ["none", "official-or-organization-managed"];
var EVENT_FIDELITY_LEVELS = ["rich", "jsonl", "message-level", "transcript"];
var APPROVAL_FIDELITY_LEVELS = [
  "native-file-diff",
  "native-codex",
  "limited",
  "none"
];

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/provider-loop-descriptor-schema.mts
var STRING_FIELDS = [
  "id",
  "displayName",
  "status",
  "providerFamily",
  "gatewayPolicy",
  "loopId",
  "eventFidelity",
  "approvalFidelity"
];
var BOOLEAN_FIELDS = ["supportsSteer", "supportsInterrupt", "supportsResume"];
var KNOWN_STRING_CHECKS = [
  { field: "status", values: new Set(DESCRIPTOR_STATUSES) },
  { field: "providerFamily", values: new Set(PROVIDER_FAMILIES) },
  { field: "gatewayPolicy", values: new Set(GATEWAY_POLICIES) },
  { field: "eventFidelity", values: new Set(EVENT_FIDELITY_LEVELS) },
  { field: "approvalFidelity", values: new Set(APPROVAL_FIDELITY_LEVELS) }
];
var CREDENTIAL_SOURCE_SET = new Set(CREDENTIAL_SOURCES);
var UNSUPPORTED_CREDENTIAL_SOURCE_SET = new Set(
  UNSUPPORTED_CREDENTIAL_SOURCES
);
var SECRET_ASSIGNMENT_PATTERN = /\b((?:[A-Z0-9_]*_)?(?:API[_-]?KEY|TOKEN|PASSWORD|SECRET))\s*[:=]\s*(?:Bearer\s+)?[A-Za-z0-9._~+/=-]{8,}/gi;
var SECRET_VALUE_PATTERNS = [
  /\bBearer\s+[A-Za-z0-9._~+/=-]{8,}/gi,
  /\bsk-(?:ant|proj)?-[A-Za-z0-9_-]{8,}/gi,
  /\bgh[opsu]_[A-Za-z0-9_]{8,}/gi,
  /\bAKIA[0-9A-Z]{16}\b/g,
  /-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g
];
function validateProviderLoopDescriptors(descriptors) {
  return descriptors.flatMap((descriptor) => validateProviderLoopDescriptor(descriptor));
}
function credentialSourcesForProjection(descriptor) {
  const projected = [];
  for (const source of descriptor.allowedCredentialSources ?? []) {
    if (isCredentialSource(source) && !projected.includes(source)) projected.push(source);
  }
  return projected;
}
function hasSecretLikeText(value) {
  if (secretPatternMatches(SECRET_ASSIGNMENT_PATTERN, value)) return true;
  return SECRET_VALUE_PATTERNS.some((pattern) => secretPatternMatches(pattern, value));
}
function redactSecretLikeText(value) {
  let redacted = value.replace(
    SECRET_ASSIGNMENT_PATTERN,
    (_match, keyName) => `${keyName}=[redacted]`
  );
  for (const pattern of SECRET_VALUE_PATTERNS) {
    pattern.lastIndex = 0;
    redacted = redacted.replace(pattern, "[redacted]");
  }
  return redacted;
}
function validateProviderLoopDescriptor(descriptor) {
  const descriptorId = descriptorIdForIssue(descriptor);
  const issues = [];
  for (const field of STRING_FIELDS) {
    const value = descriptor[field];
    if (typeof value !== "string" || value.trim() === "") {
      issues.push({
        descriptorId,
        field,
        code: "missing-field",
        message: "required string field is missing"
      });
      continue;
    }
    if (hasSecretLikeText(value)) {
      issues.push({
        descriptorId,
        field,
        code: "secret-like-value",
        message: "field contains a secret-like value"
      });
    }
  }
  for (const field of BOOLEAN_FIELDS) {
    if (typeof descriptor[field] !== "boolean") {
      issues.push({
        descriptorId,
        field,
        code: "invalid-value",
        message: "required boolean field is invalid"
      });
    }
  }
  for (const check of KNOWN_STRING_CHECKS) {
    const value = descriptor[check.field];
    if (typeof value === "string" && value.trim() && !check.values.has(value)) {
      issues.push({
        descriptorId,
        field: check.field,
        code: "invalid-value",
        message: `${value} is not a supported ${check.field} value`
      });
    }
  }
  validateCredentialSources(descriptorId, descriptor.allowedCredentialSources, issues);
  validateUnsupportedCredentialSources(
    descriptorId,
    descriptor.unsupportedCredentialSources,
    issues
  );
  validateComplianceNotes(descriptorId, descriptor.complianceNotes, issues);
  return issues;
}
function validateCredentialSources(descriptorId, sources, issues) {
  if (!sources) {
    issues.push({
      descriptorId,
      field: "allowedCredentialSources",
      code: "missing-field",
      message: "allowed credential sources are missing"
    });
    return;
  }
  for (const source of sources) {
    if (isUnsupportedCredentialSource(source)) {
      issues.push({
        descriptorId,
        field: "allowedCredentialSources",
        code: "unsupported-credential-source",
        message: `${source} cannot be projected as supported`
      });
    } else if (!isCredentialSource(source)) {
      issues.push({
        descriptorId,
        field: "allowedCredentialSources",
        code: "invalid-value",
        message: `${source} is not a supported credential source label`
      });
    }
  }
}
function validateUnsupportedCredentialSources(descriptorId, sources, issues) {
  if (!sources) {
    issues.push({
      descriptorId,
      field: "unsupportedCredentialSources",
      code: "missing-field",
      message: "unsupported credential sources are missing"
    });
    return;
  }
  for (const source of sources) {
    if (!isUnsupportedCredentialSource(source)) {
      issues.push({
        descriptorId,
        field: "unsupportedCredentialSources",
        code: "invalid-value",
        message: `${source} is not an unsupported credential source label`
      });
    }
  }
}
function validateComplianceNotes(descriptorId, notes, issues) {
  if (!notes) {
    issues.push({
      descriptorId,
      field: "complianceNotes",
      code: "missing-field",
      message: "compliance notes are missing"
    });
    return;
  }
  for (const note of notes) {
    if (hasSecretLikeText(note)) {
      issues.push({
        descriptorId,
        field: "complianceNotes",
        code: "secret-like-value",
        message: "compliance note contains a secret-like value"
      });
    }
  }
}
function descriptorIdForIssue(descriptor) {
  return typeof descriptor.id === "string" && descriptor.id.trim() ? descriptor.id : "<unknown>";
}
function secretPatternMatches(pattern, value) {
  pattern.lastIndex = 0;
  return pattern.test(value);
}
function isCredentialSource(value) {
  return CREDENTIAL_SOURCE_SET.has(value);
}
function isUnsupportedCredentialSource(value) {
  return UNSUPPORTED_CREDENTIAL_SOURCE_SET.has(value);
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/provider-loop-descriptors.mts
var PROVIDER_LOOP_DESCRIPTORS = [
  {
    id: "claude-code",
    displayName: "Claude Code",
    status: "stable",
    providerFamily: "anthropic",
    allowedCredentialSources: [
      "user-api-key",
      "cloud-provider",
      "organization-gateway",
      "local-cli-auth"
    ],
    gatewayPolicy: "official-or-organization-managed",
    loopId: "native-claude-code-sdk",
    eventFidelity: "rich",
    approvalFidelity: "native-file-diff",
    supportsSteer: true,
    supportsInterrupt: true,
    supportsResume: true,
    complianceNotes: [
      "Credentials stay on the user host, cloud provider chain, or organization gateway.",
      "Local CLI auth is local-only and not a productized credential distribution model."
    ],
    unsupportedCredentialSources: UNSUPPORTED_CREDENTIAL_SOURCES
  },
  {
    id: "codex",
    displayName: "Codex proxy",
    status: "stable",
    providerFamily: "openai",
    allowedCredentialSources: ["local-cli-auth"],
    gatewayPolicy: "none",
    loopId: "codex-jsonl-proxy",
    eventFidelity: "jsonl",
    approvalFidelity: "native-codex",
    supportsSteer: false,
    supportsInterrupt: true,
    supportsResume: true,
    complianceNotes: [
      "Auth remains owned by the local Codex CLI on the user host.",
      "The adapter does not collect or redistribute Codex subscription/session credentials."
    ],
    unsupportedCredentialSources: UNSUPPORTED_CREDENTIAL_SOURCES
  }
];

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/provider-loop-selection.mts
var PROVIDER_LOOP_SELECTION_CONFIG_KEYS = [
  "provider_loop_provider",
  "providerLoopProvider",
  "provider_loop_config.providerId",
  "provider_loop_agent_loop",
  "providerLoopAgentLoop",
  "provider_loop_config.loopId"
];
function resolveProviderLoopSelection(input = {}, descriptors = PROVIDER_LOOP_DESCRIPTORS) {
  const defaultDescriptor = descriptors[0];
  if (!defaultDescriptor) {
    throw new Error("provider loop descriptors are empty");
  }
  const providerRaw = inputText(input.providerId);
  const loopRaw = inputText(input.loopId);
  const selectedByProvider = providerRaw ? descriptors.find((descriptor2) => descriptor2.id === providerRaw) : null;
  const selectedByLoop = loopRaw ? descriptors.find((descriptor2) => descriptor2.loopId === loopRaw) : null;
  const issues = selectionIssues(providerRaw, loopRaw, selectedByProvider, selectedByLoop);
  const descriptor = issues.length === 0 ? selectedByProvider ?? selectedByLoop ?? defaultDescriptor : defaultDescriptor;
  const runtimeType = input.mock ? "mock" : input.legacyRuntimeType ?? runtimeTypeForProviderLoop(descriptor);
  return {
    providerId: descriptor.id,
    loopId: descriptor.loopId,
    runtimeType,
    source: selectionSource(input, providerRaw, loopRaw),
    issues
  };
}
function providerLoopSelectionInputFromEnv(env, legacyRuntimeType = null) {
  return {
    providerId: env.CLAUDE_CODEX_PROVIDER,
    loopId: env.CLAUDE_CODEX_AGENT_LOOP,
    legacyRuntimeType,
    mock: env.CLAUDE_CODEX_MOCK === "1",
    source: "environment"
  };
}
function providerLoopSelectionInputFromConfig(config, legacyRuntimeType = null, mock = false) {
  return {
    providerId: config.provider_loop_provider ?? config.providerLoopProvider ?? config["provider_loop_config.providerId"],
    loopId: config.provider_loop_agent_loop ?? config.providerLoopAgentLoop ?? config["provider_loop_config.loopId"],
    legacyRuntimeType,
    mock,
    source: "config"
  };
}
function hasProviderLoopSelectionInput(input) {
  return inputText(input.providerId) !== null || inputText(input.loopId) !== null;
}
function isProviderLoopSelectionConfigKey(key) {
  return PROVIDER_LOOP_SELECTION_CONFIG_KEYS.some((candidate) => candidate === key);
}
function inputText(value) {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : null;
}
function selectionIssues(providerRaw, loopRaw, selectedByProvider, selectedByLoop) {
  const issues = [];
  if (providerRaw && !selectedByProvider) {
    issues.push({
      descriptorId: "provider-loop-selection",
      field: "provider",
      code: "unknown-provider",
      message: `unknown provider selection: ${redactSecretLikeText(providerRaw)}`
    });
  }
  if (loopRaw && !selectedByLoop) {
    issues.push({
      descriptorId: "provider-loop-selection",
      field: "loop",
      code: "unknown-loop",
      message: `unknown agent loop selection: ${redactSecretLikeText(loopRaw)}`
    });
  }
  if (selectedByProvider && selectedByLoop && selectedByProvider.id !== selectedByLoop.id) {
    issues.push({
      descriptorId: "provider-loop-selection",
      field: "loop",
      code: "provider-loop-mismatch",
      message: `provider ${selectedByProvider.id} does not support agent loop ${selectedByLoop.loopId}`
    });
  }
  return issues;
}
function runtimeTypeForProviderLoop(descriptor) {
  if (descriptor.id === "codex" && descriptor.loopId === "codex-jsonl-proxy") return "codex-proxy";
  return "agent-sdk-sidecar";
}
function selectionSource(input, providerRaw, loopRaw) {
  if (input.mock) return "mock";
  if (input.legacyRuntimeType) return "legacy-runtime";
  if (providerRaw || loopRaw) return input.source ?? "environment";
  return "default";
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/provider-loop-config.mts
function projectProviderLoopConfig(descriptors = PROVIDER_LOOP_DESCRIPTORS, selectionInput = {}) {
  const selection = resolveProviderLoopSelection(selectionInput);
  return {
    providers: descriptors.flatMap((descriptor) => projectDescriptor(descriptor)),
    selection: {
      providerId: safeText(selection.providerId),
      loopId: safeText(selection.loopId),
      runtimeType: selection.runtimeType,
      source: selection.source
    },
    issues: [
      ...validateProviderLoopDescriptors(descriptors).map(projectValidationIssue),
      ...selection.issues.map(projectSelectionIssue)
    ]
  };
}
function projectDescriptor(descriptor) {
  if (!isProjectableDescriptor(descriptor)) return [];
  return [
    {
      id: safeText(descriptor.id),
      displayName: safeText(descriptor.displayName),
      status: safeText(descriptor.status),
      providerFamily: safeText(descriptor.providerFamily),
      loopId: safeText(descriptor.loopId),
      eventFidelity: safeText(descriptor.eventFidelity),
      approvalFidelity: safeText(descriptor.approvalFidelity),
      supportsSteer: descriptor.supportsSteer,
      supportsInterrupt: descriptor.supportsInterrupt,
      supportsResume: descriptor.supportsResume,
      allowedCredentialSources: credentialSourcesForProjection(descriptor),
      gatewayPolicy: safeText(descriptor.gatewayPolicy),
      complianceNotes: descriptor.complianceNotes.map(safeText)
    }
  ];
}
function isProjectableDescriptor(descriptor) {
  return typeof descriptor.id === "string" && typeof descriptor.displayName === "string" && typeof descriptor.status === "string" && typeof descriptor.providerFamily === "string" && typeof descriptor.gatewayPolicy === "string" && typeof descriptor.loopId === "string" && typeof descriptor.eventFidelity === "string" && typeof descriptor.approvalFidelity === "string" && typeof descriptor.supportsSteer === "boolean" && typeof descriptor.supportsInterrupt === "boolean" && typeof descriptor.supportsResume === "boolean" && Array.isArray(descriptor.allowedCredentialSources) && Array.isArray(descriptor.complianceNotes);
}
function safeText(value) {
  return redactSecretLikeText(value);
}
function projectValidationIssue(issue) {
  return {
    descriptorId: safeText(issue.descriptorId),
    field: issue.field,
    code: issue.code,
    message: safeText(issue.message)
  };
}
function projectSelectionIssue(issue) {
  return {
    descriptorId: "provider-loop-selection",
    field: issue.field,
    code: issue.code,
    message: safeText(issue.message)
  };
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/run-registry.mts
import { appendFileSync as appendFileSync2 } from "node:fs";
import { join as join5, resolve as resolve2 } from "node:path";
function runRegistryPath(env = process.env) {
  const configured = env.CLAUDE_CODEX_RUN_LOG;
  if (configured === "0" || configured === "false") return null;
  return resolve2(configured || join5(adapterHome(), "runs.jsonl"));
}
function recordRunEvent(event, data = {}, env = process.env) {
  const path = runRegistryPath(env);
  if (!path) return { ok: true, path: null, error: null };
  try {
    ensureParent(path);
    appendFileSync2(path, `${JSON.stringify(runRegistryEntry(event, data))}
`, {
      mode: 384
    });
    return { ok: true, path, error: null };
  } catch (error) {
    if (error instanceof Error) return { ok: false, path, error: error.message };
    throw error;
  }
}
function runRegistryEntry(event, data = {}) {
  return {
    ts: (/* @__PURE__ */ new Date()).toISOString(),
    pid: process.pid,
    event,
    ...redactRecord(data)
  };
}
function redactRunRegistryData(value) {
  if (value === null) return null;
  if (Array.isArray(value)) return value.map((item) => redactRunRegistryData(item));
  if (typeof value === "object") return redactRecord(value);
  if (typeof value === "string") return redactString(value);
  return value;
}
function redactRecord(value) {
  const redacted = {};
  for (const [key, child] of Object.entries(value)) {
    redacted[key] = sensitiveKeyPattern.test(key) ? "[redacted]" : redactRunRegistryData(child);
  }
  return redacted;
}
function redactString(value) {
  return value.replace(secretLikePattern, "$1=[redacted]");
}
var sensitiveKeyPattern = /prompt|input|output|response|content|text|secret|token|key|password|authorization/i;
var secretLikePattern = /\b(api[_-]?key|token|secret|password|authorization)\s*=\s*([A-Za-z0-9._~+/=-]{8,})/gi;

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/runtime-config.mts
function normalizeRuntimeType(value) {
  const raw = (value ?? "").trim().toLowerCase();
  if (!raw) return null;
  switch (raw) {
    case "mock":
      return "mock";
    case "sdk":
    case "agent-sdk":
    case "agent-sdk-sidecar":
    case "sidecar":
    case "cloud-agent-sdk":
      return "agent-sdk-sidecar";
    case "agent-sdk-socket":
    case "socket":
    case "runtime-socket":
      return "agent-sdk-sidecar";
    case "agent-http":
    case "channels":
    case "channel":
    case "http-channel":
      return "agent-http";
    case "agentapi":
    case "agent-api":
      return "agentapi";
    case "claude-p":
    case "claudep":
    case "pty-transcript":
      return "claude-p";
    case "codex-proxy":
    case "codex-exec":
      return "codex-proxy";
    default:
      throw new Error(`unknown CLAUDE_CODEX_RUNTIME_TYPE: ${value}`);
  }
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/server-helpers.mts
import { execFile } from "node:child_process";
import { promisify } from "node:util";
var execFileAsync = promisify(execFile);
function asRecord3(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}
function stringOr2(value, fallback) {
  return typeof value === "string" && value.length > 0 ? value : fallback;
}
function summarizeInjectedItem(raw) {
  if (raw == null) return "";
  if (typeof raw === "string") return raw.slice(0, 1e3);
  if (typeof raw !== "object") return String(raw).slice(0, 1e3);
  const rec = raw;
  const content = rec.content;
  if (Array.isArray(content)) {
    const texts = [];
    for (const block of content) {
      if (block && typeof block === "object") {
        const b = block;
        if (typeof b.text === "string") texts.push(b.text);
      }
    }
    if (texts.length > 0) return texts.join("\n").slice(0, 2e3);
  }
  if (typeof rec.text === "string") return rec.text.slice(0, 2e3);
  try {
    return JSON.stringify(raw).slice(0, 500);
  } catch {
    return "[non-serializable item]";
  }
}
function configEdits(params) {
  if (Array.isArray(params.edits)) {
    return params.edits.map((edit) => {
      const rec = asRecord3(edit);
      return { keyPath: String(rec.keyPath ?? rec.key ?? ""), value: rec.value };
    }).filter((edit) => edit.keyPath.length > 0);
  }
  const keyPath = String(params.keyPath ?? params.key ?? "");
  return keyPath ? [{ keyPath, value: params.value }] : [];
}
function configLayerMetadata() {
  return {
    name: { type: "user", file: `${codexHome()}/config.toml` },
    version: `claude-codex-${nowSeconds()}`
  };
}
function allSelectableModelOptions() {
  return [...claudeModelOptions(), ...codexProxyModelOptions()];
}
function defaultSelectableModelId() {
  const options = allSelectableModelOptions();
  const defaultModel = process.env.CLAUDE_CODEX_DEFAULT_MODEL;
  if (defaultModel && process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY === "1" && !isCodexOpenAiModel(defaultModel)) return defaultModel;
  if (defaultModel && options.some((option) => option.id === defaultModel)) return defaultModel;
  return options.find((option) => option.isDefault === true)?.id ?? options[0]?.id ?? "sonnet";
}
function normalizeSelectableModelId(value, fallback) {
  if (value.trim() && process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY === "1" && !isCodexOpenAiModel(value)) return value;
  const options = allSelectableModelOptions();
  const ids = new Set(options.map((option) => option.id));
  if (ids.has(value)) return value;
  const matched = options.find((opt) => opt.id.toLowerCase() === value.toLowerCase());
  if (matched) return matched.id;
  if (ids.has(fallback)) return fallback;
  const defaultModel = defaultSelectableModelId();
  debugLog("config.model.repaired", { requestedModel: value, repairedModel: defaultModel });
  return defaultModel;
}
function modelFromParams(params, fallback) {
  const config = asRecord3(params.config);
  const settings = asRecord3(asRecord3(params.collaborationMode).settings);
  if (typeof settings.model === "string" && settings.model.length > 0) return settings.model;
  if (typeof params.model === "string" && params.model.length > 0) return params.model;
  if (typeof config.model === "string" && config.model.length > 0) return config.model;
  return fallback ?? "";
}
function reasoningEffortFromParams(params, fallback) {
  const config = asRecord3(params.config);
  const settings = asRecord3(asRecord3(params.collaborationMode).settings);
  const effort = typeof settings.reasoning_effort === "string" ? settings.reasoning_effort : typeof params.effort === "string" ? params.effort : typeof config.model_reasoning_effort === "string" ? config.model_reasoning_effort : fallback;
  return normalizeCodexReasoningEffort(effort);
}
function numberOr(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}
function commandArray(value) {
  if (Array.isArray(value)) return value.map(String).filter((part) => part.length > 0);
  if (typeof value === "string" && value.trim())
    return [process.env.SHELL || "/bin/sh", "-lc", value];
  return [];
}
function summarizeRpcParams(method, params) {
  const rec = asRecord3(params);
  if (method === "turn/start") {
    return {
      threadId: rec.threadId,
      cwd: rec.cwd,
      model: rec.model,
      effort: rec.effort,
      configEffort: asRecord3(rec.config).model_reasoning_effort,
      hasOutputSchema: rec.outputSchema != null,
      inputTypes: Array.isArray(rec.input) ? rec.input.map((item) => asRecord3(item).type) : []
    };
  }
  if (method === "process/spawn" || method === "command/exec") {
    return {
      processHandle: rec.processHandle,
      processId: rec.processId,
      cwd: rec.cwd,
      command: commandArray(rec.command),
      streamStdoutStderr: rec.streamStdoutStderr,
      streamStdin: rec.streamStdin,
      tty: rec.tty,
      timeoutMs: rec.timeoutMs
    };
  }
  if (method.includes("outputDelta")) {
    return {
      processHandle: rec.processHandle,
      processId: rec.processId,
      itemId: rec.itemId,
      stream: rec.stream,
      deltaBytes: typeof rec.deltaBase64 === "string" ? Buffer.from(rec.deltaBase64, "base64").byteLength : typeof rec.delta === "string" ? rec.delta.length : 0,
      capReached: rec.capReached
    };
  }
  if (method === "item/agentMessage/delta") {
    return {
      threadId: rec.threadId,
      turnId: rec.turnId,
      itemId: rec.itemId,
      deltaChars: typeof rec.delta === "string" ? rec.delta.length : 0
    };
  }
  if (method === "item/started" || method === "item/completed") {
    const item = asRecord3(rec.item);
    return {
      threadId: rec.threadId,
      turnId: rec.turnId,
      item: { id: item.id, type: item.type }
    };
  }
  if (method === "turn/started" || method === "turn/completed") {
    const turn = asRecord3(rec.turn);
    const items = Array.isArray(turn.items) ? turn.items.map((item) => ({ id: asRecord3(item).id, type: asRecord3(item).type })) : [];
    return {
      threadId: rec.threadId,
      turn: { id: turn.id, status: turn.status, items }
    };
  }
  return rec;
}
function reviewLabel(target) {
  const rec = asRecord3(target);
  const type = stringOr2(rec.type, "uncommittedChanges");
  if (type === "commit")
    return "commit " + stringOr2(rec.sha, "") + (typeof rec.title === "string" ? ": " + rec.title : "");
  if (type === "baseBranch") return "base branch " + stringOr2(rec.branch, "main");
  if (type === "custom") return stringOr2(rec.instructions, "custom review");
  return "uncommitted changes";
}
function reviewPrompt(target) {
  const label = reviewLabel(target);
  return [
    "Review the code changes for: " + label,
    "",
    "Prioritize correctness bugs, regressions, security issues, and missing tests.",
    "Return findings first, ordered by severity, with file and line references when available.",
    "If there are no actionable issues, say so clearly and mention residual risk."
  ].join("\n");
}
function compactSummary(thread, turns) {
  const snippets = turns.flatMap((turn) => turn.items).filter((item) => item.type === "userMessage" || item.type === "agentMessage").slice(-12).map((item) => {
    if (item.type === "userMessage") return "User: " + textFromInput(item.content).slice(0, 500);
    if (item.type === "agentMessage") return "Assistant: " + item.text.slice(0, 500);
    return "";
  }).filter(Boolean);
  return [
    "Context compacted for thread " + thread.id + ".",
    "",
    snippets.length > 0 ? snippets.join("\n") : "No prior conversation content was available to summarize."
  ].join("\n");
}
function commandEnv(value) {
  const env = { ...process.env };
  if (!value || typeof value !== "object" || Array.isArray(value)) return env;
  for (const [key, raw] of Object.entries(value)) {
    if (raw == null) delete env[key];
    else env[key] = String(raw);
  }
  return env;
}
function stringListFromEnv(name2, fallback) {
  const raw = process.env[name2];
  if (!raw) return fallback;
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.map(String).filter(Boolean);
  } catch {
  }
  return raw.split(",").map((part) => part.trim()).filter(Boolean);
}
function toolResultText(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content.map((entry) => {
      if (typeof entry === "string") return entry;
      if (entry && typeof entry === "object") {
        const record = entry;
        if (typeof record.text === "string") return record.text;
        if (typeof record.content === "string") return record.content;
      }
      return "";
    }).filter(Boolean).join("\n");
  }
  if (content && typeof content === "object") {
    const record = content;
    if (typeof record.text === "string") return record.text;
    if (typeof record.content === "string") return record.content;
  }
  return "";
}
function fallbackStructuredText(outputSchema, prompt) {
  return JSON.stringify(coerceStructuredValue(outputSchema, prompt), null, 0);
}
function isSubagentToolName(name2) {
  if (typeof name2 !== "string") return false;
  const n = name2.trim().toLowerCase();
  return n === "task" || n === "agent" || n === "subagent" || n === "spawn_agent" || n === "spawnagent";
}
function normalizeApprovalPolicy(value) {
  if (typeof value !== "string") return null;
  const v = value.trim();
  if (v === "untrusted" || v === "on-failure" || v === "on-request" || v === "never") return v;
  if (v === "unless-trusted") return "untrusted";
  return null;
}
function normalizeSandboxMode(value) {
  if (typeof value !== "string") return null;
  const v = value.trim();
  return v === "read-only" || v === "workspace-write" || v === "danger-full-access" ? v : null;
}
function normalizePermissionProfileId(value) {
  if (typeof value !== "string") return null;
  const id = value.trim();
  return id.length > 0 && id.length <= 128 ? id : null;
}
function permissionProfileIdFromParams(params) {
  const direct = normalizePermissionProfileId(params.permissions);
  if (direct) return direct;
  const active = asRecord3(params.activePermissionProfile);
  return normalizePermissionProfileId(active.id);
}
function hasLegacyPermissionParams(params) {
  return typeof params.approvalPolicy === "string" || typeof params.sandbox === "string" || params.sandboxPolicy !== null && typeof params.sandboxPolicy === "object" && !Array.isArray(params.sandboxPolicy);
}
function permissionProfilePolicy(value) {
  const id = normalizePermissionProfileId(value);
  if (!id) return null;
  switch (id) {
    case ":read-only":
      return { id, approvalPolicy: "on-request", sandboxMode: "read-only" };
    case ":workspace":
      return { id, approvalPolicy: "on-request", sandboxMode: "workspace-write" };
    case ":danger-full-access":
      return { id, approvalPolicy: "never", sandboxMode: "danger-full-access" };
    default:
      return { id, approvalPolicy: null, sandboxMode: null };
  }
}
function threadPermissionProfileId(permissionProfileId, approvalPolicy, sandboxMode) {
  const explicit = normalizePermissionProfileId(permissionProfileId);
  if (explicit) return explicit;
  if (sandboxMode === "read-only") return ":read-only";
  if (sandboxMode === "danger-full-access") return ":danger-full-access";
  if (sandboxMode === "workspace-write" && approvalPolicy === "on-request") return ":workspace";
  return null;
}
function sandboxFromTurnParams(params) {
  if (typeof params.sandbox === "string") return normalizeSandboxMode(params.sandbox);
  const policy = params.sandboxPolicy;
  if (!policy || typeof policy !== "object" || Array.isArray(policy)) return null;
  const type = policy.type;
  if (type === "dangerFullAccess") return "danger-full-access";
  if (type === "readOnly") return "read-only";
  if (type === "workspaceWrite" || type === "externalSandbox") return "workspace-write";
  return null;
}
function normalizePersonality(value) {
  if (typeof value !== "string") return null;
  const v = value.trim();
  if (v === "none" || v === "friendly" || v === "pragmatic" || v === "cynic" || v === "robot" || v === "nerd")
    return v;
  return null;
}
function normalizeReasoningEffortEnum(value) {
  if (typeof value !== "string") return null;
  const v = value.trim();
  if (v === "none" || v === "minimal" || v === "low" || v === "medium" || v === "high" || v === "xhigh")
    return v;
  return null;
}
function normalizeThreadSource(value) {
  if (typeof value !== "string") return null;
  const v = value.trim();
  if (v === "user" || v === "subagent" || v === "memory_consolidation") return v;
  return null;
}
function normalizeSessionSource(value) {
  if (typeof value !== "string") return "appServer";
  const v = value.trim();
  if (v === "cli" || v === "vscode" || v === "exec" || v === "appServer" || v === "unknown")
    return v;
  if (v === "app_server" || v === "app-server") return "appServer";
  return "unknown";
}
function nullIfEmpty(value) {
  if (value == null) return null;
  return value === "" ? null : value;
}
var desktopPresentationPrompt = [
  "# Desktop conversation and presentation",
  "Use Claude Desktop as the reference for conversational style: warm, direct, and thoughtful. Follow the intent of the request: discuss ideas and explain concepts when asked; carry out and verify work when asked to act.",
  "Your replies appear in Codex desktop, which renders rich Markdown and fenced Mermaid diagrams.",
  "For this desktop client, prioritize conversational clarity over terminal-oriented terseness. Open directly with substantive content; express warmth through attentiveness and useful context. Develop the explanation in connected paragraphs with concrete examples where helpful. Give complex questions enough detail to make sense; keep simple exchanges brief.",
  "Ground advice in the circumstances the user gives and explain relevant trade-offs. Distinguish facts, assumptions, and recommendations. Qualify claims when conditions matter, so an accessible simplification or a reasonable recommendation does not become a universal rule.",
  "Choose structure to suit the content: headings for substantial sections, lists for distinct points or steps, and tables for useful comparisons. Use emphasis selectively. Avoid a fixed report template, repetitive preambles, generic praise, and routine closing offers.",
  "When a flow, architecture, relationship, or sequence is clearer visually, include a compact Mermaid diagram with a concise explanation. Use a lowercase mermaid code-fence language and close the fence. Prefer simple syntax and ASCII node identifiers; double-quote node labels containing Chinese or special characters.",
  "During tool work, give brief progress updates at meaningful transitions. Let the native tool events show execution details, and explain their significance in prose. Finish with the result and relevant verification; omit a recap of the command or tool sequence already visible in the timeline unless the user asks for it.",
  "Match the language and level of detail requested. Skip diagrams for trivial replies, and follow the requested text-only preference and output format. These presentation defaults preserve the existing tool-use behavior and explicit project, developer, and personality instructions.",
  "Existing generated images can be embedded with Markdown image syntax and an absolute file path. Raw HTML, SVG, and React code are not automatically executable artifacts; do not promise an interactive artifact merely by emitting its source."
].join("\n");
function buildSystemPromptAddendum(input) {
  const sections = [];
  if (input.desktopPresentation) sections.push(desktopPresentationPrompt);
  const base = (input.baseInstructions ?? "").trim();
  if (base) sections.push(`# Project instructions
${base}`);
  const dev = (input.developerInstructions ?? "").trim();
  if (dev) sections.push(`# Developer instructions
${dev}`);
  const cue = personalityPromptCue(input.personality);
  if (cue) sections.push(cue);
  return sections.length > 0 ? sections.join("\n\n") : null;
}
function personalityPromptCue(personality) {
  switch (personality) {
    case "friendly":
      return "Personality: friendly. Communicate in a warm, encouraging tone; default to plain language and short paragraphs.";
    case "pragmatic":
      return "Personality: pragmatic. Be concise and direct; lead with the answer, skip pleasantries, prefer concrete code or commands.";
    case "cynic":
      return "Personality: cynic. Be terse and wry; surface trade-offs and risks plainly; avoid hype.";
    case "robot":
      return "Personality: robot. Reply in clipped, structured prose; prefer bullet points and exact field names; minimise filler.";
    case "nerd":
      return "Personality: nerd. Get into mechanism and detail; explain underlying assumptions and edge cases when relevant.";
    case "none":
    case null:
    default:
      return null;
  }
}
function sandboxEnvelope(mode, cwd) {
  if (mode === "read-only") return { type: "readOnly", networkAccess: false };
  if (mode === "danger-full-access") return { type: "dangerFullAccess" };
  return {
    type: "workspaceWrite",
    writableRoots: [cwd],
    networkAccess: true,
    excludeTmpdirEnvVar: false,
    excludeSlashTmp: false
  };
}
function permissionProfileList(params) {
  const profiles = [
    { id: ":read-only", description: null, allowed: true },
    { id: ":workspace", description: null, allowed: true },
    { id: ":danger-full-access", description: null, allowed: true }
  ];
  const rawCursor = params.cursor;
  const start = rawCursor == null ? 0 : Number(rawCursor);
  if (!Number.isInteger(start) || start < 0 || start > profiles.length) {
    throw new Error("invalid permission profile cursor");
  }
  const rawLimit = params.limit;
  const requestedLimit = typeof rawLimit === "number" && Number.isFinite(rawLimit) ? rawLimit : null;
  const limit = requestedLimit == null ? profiles.length : Math.max(1, Math.floor(requestedLimit));
  const end = Math.min(profiles.length, start + limit);
  return {
    data: profiles.slice(start, end),
    nextCursor: end < profiles.length ? String(end) : null
  };
}
function emptyTokenBreakdown() {
  return {
    totalTokens: 0,
    inputTokens: 0,
    cachedInputTokens: 0,
    outputTokens: 0,
    reasoningOutputTokens: 0
  };
}
function wrapMcpToolResult(content) {
  if (content == null) return { content: [], structuredContent: null, _meta: null };
  if (Array.isArray(content)) return { content, structuredContent: null, _meta: null };
  const text = typeof content === "string" ? content : JSON.stringify(content);
  return {
    content: [{ type: "text", text }],
    structuredContent: typeof content === "object" ? content : null,
    _meta: null
  };
}
function wrapMcpToolError(content) {
  if (content == null) return { message: "tool call failed" };
  if (typeof content === "string") return { message: content };
  if (Array.isArray(content)) {
    const text = content.map(
      (b) => b && typeof b === "object" && b.type === "text" ? String(b.text ?? "") : ""
    ).filter(Boolean).join("\n");
    return { message: text || JSON.stringify(content) };
  }
  if (typeof content === "object" && content && typeof content.message === "string") {
    return { message: content.message };
  }
  return { message: JSON.stringify(content) };
}
function parseSubagentTrailer(text) {
  if (!text) return { cleanText: "", agentId: null, usage: null };
  let cleanText = text;
  let agentId = null;
  let usage = null;
  const usageMatch = cleanText.match(
    /<usage>\s*total_tokens:\s*(\d+)\s*\n\s*tool_uses:\s*(\d+)\s*\n\s*duration_ms:\s*(\d+)\s*<\/usage>\s*$/
  );
  if (usageMatch) {
    usage = {
      totalTokens: Number(usageMatch[1]) || 0,
      toolUses: Number(usageMatch[2]) || 0,
      durationMs: Number(usageMatch[3]) || 0
    };
    cleanText = cleanText.slice(0, usageMatch.index).replace(/\s+$/, "");
  }
  const agentMatch = cleanText.match(/\n?agentId:\s*([0-9a-f]{8,32})\b[^\n]*\s*$/i);
  if (agentMatch) {
    agentId = agentMatch[1] ?? null;
    cleanText = cleanText.slice(0, agentMatch.index).replace(/\s+$/, "");
  }
  return { cleanText, agentId, usage };
}
function todoWriteToPlanSteps(input) {
  const todos = input.todos;
  if (!Array.isArray(todos)) return null;
  return todos.map((raw) => {
    const todo = raw && typeof raw === "object" ? raw : {};
    const step = typeof todo.content === "string" && todo.content.length > 0 ? todo.content : typeof todo.activeForm === "string" ? todo.activeForm : "";
    const status = todo.status === "in_progress" ? "inProgress" : todo.status === "completed" ? "completed" : "pending";
    return { step, status };
  });
}
function parseExitCodeFromResult(content) {
  const text = typeof content === "string" ? content : Array.isArray(content) ? content.map((p) => typeof p === "string" ? p : p?.text ?? "").join("") : "";
  if (!text) return null;
  const match = text.match(/(?:Exit code|exit status|exit code):\s*(-?\d+)/i);
  if (!match) return null;
  const code = Number(match[1]);
  return Number.isFinite(code) ? code : null;
}
function parseWebSearchAction(query, resultText2) {
  if (!resultText2) return { type: "search", query: query || null, queries: null };
  const urlMatch = resultText2.match(/https?:\/\/[^\s)\]"'<]+/);
  if (urlMatch && query) return { type: "openPage", url: urlMatch[0] };
  return { type: "search", query: query || null, queries: null };
}
function tokenBreakdownFromClaudeUsage(usage) {
  const num = (value) => typeof value === "number" && Number.isFinite(value) ? value : 0;
  const cacheRead = num(usage.cache_read_input_tokens);
  const cacheCreation = num(usage.cache_creation_input_tokens);
  const inputTokens = num(usage.input_tokens) + cacheCreation;
  const outputTokens = num(usage.output_tokens);
  return {
    inputTokens,
    cachedInputTokens: cacheRead,
    outputTokens,
    reasoningOutputTokens: 0,
    totalTokens: inputTokens + cacheRead + outputTokens
  };
}
function coerceStructuredValue(schema, prompt) {
  if (!schema || typeof schema !== "object" || Array.isArray(schema)) return prompt;
  const record = schema;
  if (record.type === "string") return conciseStructuredString(prompt);
  if (record.type === "array") {
    const itemSchema = record.items && typeof record.items === "object" && !Array.isArray(record.items) ? record.items : { type: "string" };
    const values = prompt.split(/\r?\n/).map((line) => line.trim().replace(/^[-*\d.、)\s]+/, "")).filter(Boolean);
    return (values.length > 0 ? values : prompt.trim() ? [prompt.trim()] : []).slice(0, 10).map((value) => coerceStructuredValue(itemSchema, value));
  }
  if (record.type !== "object") return null;
  const properties = record.properties && typeof record.properties === "object" && !Array.isArray(record.properties) ? record.properties : {};
  const required = Array.isArray(record.required) ? record.required.map(String) : Object.keys(properties);
  const result = {};
  for (const key of required) {
    const property = properties[key];
    result[key] = coerceStructuredValue(property, prompt);
  }
  return result;
}
function conciseStructuredString(prompt) {
  const lines = prompt.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const source = lines.at(-1) ?? prompt.trim();
  const colon = Math.max(source.lastIndexOf("\uFF1A"), source.lastIndexOf(":"));
  const value = colon >= 0 ? source.slice(colon + 1).trim() : source;
  return value.replace(/^[-*\d.、)\s]+/, "").slice(0, 80).trim();
}
function normalizeDecision(response) {
  const decision = asRecord3(response).decision;
  if (decision === "accept" || decision === "acceptForSession" || decision === "decline" || decision === "cancel")
    return decision;
  if (decision && typeof decision === "object" && ("acceptWithExecpolicyAmendment" in decision || "applyNetworkPolicyAmendment" in decision)) {
    return "acceptForSession";
  }
  return "decline";
}
function fileChangeFromTool(toolName, input) {
  if (toolName === "MultiEdit" && Array.isArray(input.edits)) {
    return input.edits.map((edit, index) => {
      const rec = asRecord3(edit);
      return {
        path: String(input.file_path ?? input.path ?? `edit-${index}`),
        kind: { type: "update", move_path: null },
        diff: simpleDiff(
          String(input.file_path ?? input.path ?? `edit-${index}`),
          String(rec.old_string ?? ""),
          String(rec.new_string ?? "")
        )
      };
    });
  }
  const path = String(input.file_path ?? input.path ?? input.filename ?? "unknown");
  if (toolName === "Write") {
    return [
      { path, kind: { type: "add" }, diff: simpleDiff(path, "", String(input.content ?? "")) }
    ];
  }
  return [
    {
      path,
      kind: { type: "update", move_path: null },
      diff: simpleDiff(path, String(input.old_string ?? ""), String(input.new_string ?? ""))
    }
  ];
}
function simpleDiff(path, oldText, newText) {
  return [
    `--- a/${path}`,
    `+++ b/${path}`,
    "@@",
    ...oldText.split("\n").filter(Boolean).map((line) => `-${line}`),
    ...newText.split("\n").filter(Boolean).map((line) => `+${line}`),
    ""
  ].join("\n");
}
function isNotAGitRepo(error) {
  const message = error instanceof Error ? error.message : String(error);
  return /not a git repository/i.test(message);
}
var gitRepoCache = /* @__PURE__ */ new Map();
async function isGitWorkTree(cwd) {
  if (gitRepoCache.has(cwd)) return gitRepoCache.get(cwd);
  let inside = false;
  try {
    const { stdout } = await execFileAsync("git", ["rev-parse", "--is-inside-work-tree"], {
      cwd,
      timeout: 3e3,
      maxBuffer: 1024
    });
    inside = stdout.trim() === "true";
  } catch {
    inside = false;
  }
  gitRepoCache.set(cwd, inside);
  return inside;
}
async function gitDiff(cwd) {
  if (!await isGitWorkTree(cwd)) return "";
  let trackedDiff = "";
  try {
    const { stdout } = await execFileAsync("git", ["diff", "--no-ext-diff", "--"], {
      cwd,
      timeout: 1e4,
      maxBuffer: 5 * 1024 * 1024
    });
    trackedDiff = stdout;
  } catch (error) {
    debugLog("git.diff.failed", {
      cwd,
      reason: isNotAGitRepo(error) ? "notAGitRepository" : "error",
      error: error instanceof Error ? error.message : String(error)
    });
    return "";
  }
  const untrackedDiff = await gitUntrackedDiff(cwd);
  return [trackedDiff, untrackedDiff].filter(Boolean).join("\n");
}
async function gitUntrackedDiff(cwd) {
  try {
    const { readFile } = await import("node:fs/promises");
    const { stdout } = await execFileAsync(
      "git",
      ["ls-files", "--others", "--exclude-standard", "-z"],
      {
        cwd,
        timeout: 1e4,
        maxBuffer: 1024 * 1024
      }
    );
    const paths = stdout.split("\0").filter(Boolean).slice(0, 50);
    const diffs = [];
    for (const path of paths) {
      const bytes = await readFile(`${cwd}/${path}`);
      if (bytes.includes(0)) continue;
      diffs.push(addedFileDiff(path, bytes.toString("utf8")));
    }
    return diffs.join("\n");
  } catch (error) {
    debugLog("git.untrackedDiff.failed", {
      cwd,
      reason: isNotAGitRepo(error) ? "notAGitRepository" : "error",
      error: error instanceof Error ? error.message : String(error)
    });
    return "";
  }
}
function addedFileDiff(path, text) {
  const lines = text.length === 0 ? [] : text.replace(/\n$/, "").split("\n");
  return [
    `diff --git a/${path} b/${path}`,
    "new file mode 100644",
    "index 0000000..0000000",
    "--- /dev/null",
    `+++ b/${path}`,
    `@@ -0,0 +1,${lines.length} @@`,
    ...lines.map((line) => `+${line}`),
    ""
  ].join("\n");
}
async function listFiles(root) {
  try {
    const { stdout } = await execFileAsync("rg", ["--files"], {
      cwd: root,
      timeout: 1e4,
      maxBuffer: 5 * 1024 * 1024
    });
    return stdout.split("\n").filter(Boolean);
  } catch (rgError) {
    debugLog("listFiles.rgFailed", {
      root,
      error: rgError instanceof Error ? rgError.message : String(rgError)
    });
    try {
      const { stdout } = await execFileAsync("find", [".", "-type", "f"], {
        cwd: root,
        timeout: 1e4,
        maxBuffer: 5 * 1024 * 1024
      });
      return stdout.split("\n").filter(Boolean).map((path) => path.replace(/^\.\//, ""));
    } catch (findError) {
      debugLog("listFiles.findFailed", {
        root,
        error: findError instanceof Error ? findError.message : String(findError)
      });
      return [];
    }
  }
}
function normalizeUserInputAnswers(response, questions) {
  const empty = {
    answers: Object.fromEntries(questions.map((q) => [q.id, { answers: [] }]))
  };
  if (!response || typeof response !== "object") return empty;
  const root = response;
  const slot = root.answers ?? root;
  if (!slot || typeof slot !== "object") return empty;
  const out = {};
  for (const q of questions) {
    const raw = slot[q.id];
    if (!raw) {
      out[q.id] = { answers: [] };
      continue;
    }
    if (Array.isArray(raw)) {
      out[q.id] = { answers: raw.map(String) };
      continue;
    }
    if (typeof raw === "string") {
      out[q.id] = { answers: [raw] };
      continue;
    }
    const entry = raw;
    const list = Array.isArray(entry.answers) ? entry.answers.map(String) : [];
    const notes = typeof entry.notes === "string" ? entry.notes : null;
    out[q.id] = notes != null ? { answers: list, notes } : { answers: list };
  }
  return { answers: out };
}
function userInputAnswersAsContent(questions, answers) {
  const out = [];
  for (const q of questions) {
    const slot = answers.answers[q.id];
    const picked = slot?.answers?.filter((s) => s && s !== "Other") ?? [];
    const notes = slot?.notes ?? null;
    const body = notes && (picked.length === 0 || picked.includes("Other")) ? notes : picked.join(", ") || "(no answer)";
    out.push({ type: "inputText", text: `${q.header}: ${body}` });
  }
  return out;
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/store.mts
import { mkdirSync as mkdirSync2 } from "node:fs";
import { createRequire } from "node:module";
import { join as join6 } from "node:path";
var require2 = createRequire(import.meta.url);
var PINNED_SECTION_ID = "01984de2-8f74-7c91-a3b2-5c5e937cf318";
var PINNED_SECTION_NAME = "Pinned";
function openDatabase(path) {
  const sqlite = require2("node:sqlite");
  return new sqlite.DatabaseSync(path);
}
var SessionStore = class {
  db;
  constructor(path = join6(adapterHome(), "state.sqlite")) {
    mkdirSync2(adapterHome(), { recursive: true, mode: 448 });
    this.db = openDatabase(path);
    this.migrate();
  }
  migrate() {
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS threads (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        forked_from_id TEXT,
        is_pinned INTEGER NOT NULL DEFAULT 0,
        section_id TEXT,
        section_entered_at INTEGER,
        section_position INTEGER,
        preview TEXT NOT NULL,
        name TEXT,
        archived INTEGER NOT NULL DEFAULT 0,
        cwd TEXT NOT NULL,
        model TEXT NOT NULL,
        reasoning_effort TEXT,
        model_provider TEXT NOT NULL,
        claude_session_id TEXT,
        source TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        status_json TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS turns (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL,
        status TEXT NOT NULL,
        started_at INTEGER,
        completed_at INTEGER,
        duration_ms INTEGER,
        items_json TEXT NOT NULL,
        diff TEXT NOT NULL DEFAULT '',
        error_json TEXT,
        FOREIGN KEY(thread_id) REFERENCES threads(id)
      );
      CREATE INDEX IF NOT EXISTS idx_threads_updated ON threads(updated_at DESC);
      CREATE INDEX IF NOT EXISTS idx_turns_thread ON turns(thread_id, started_at);
      CREATE TABLE IF NOT EXISTS thread_sections (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        appearance_json TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS idx_thread_sections_created ON thread_sections(created_at, id);
    `);
    this.ensureColumn("threads", "is_pinned", "INTEGER NOT NULL DEFAULT 0");
    this.ensureColumn("threads", "section_id", "TEXT");
    this.ensureColumn("threads", "section_entered_at", "INTEGER");
    this.ensureColumn("threads", "section_position", "INTEGER");
    this.db.exec(
      "CREATE INDEX IF NOT EXISTS idx_threads_section_position ON threads(section_id, section_position, id)"
    );
    this.ensureColumn("threads", "reasoning_effort", "TEXT");
    this.ensureColumn("threads", "approval_policy", "TEXT");
    this.ensureColumn("threads", "sandbox_mode", "TEXT");
    this.ensureColumn("threads", "permission_profile_id", "TEXT");
    this.ensureColumn("threads", "collaboration_mode", "TEXT NOT NULL DEFAULT 'default'");
    this.ensureColumn("threads", "ephemeral", "INTEGER NOT NULL DEFAULT 0");
    this.ensureColumn("threads", "thread_source", "TEXT");
    this.ensureColumn("threads", "agent_role", "TEXT");
    this.ensureColumn("threads", "agent_nickname", "TEXT");
    this.ensureColumn("threads", "base_instructions", "TEXT");
    this.ensureColumn("threads", "developer_instructions", "TEXT");
    this.ensureColumn("threads", "personality", "TEXT");
    this.ensureColumn("threads", "runtime_backend", "TEXT NOT NULL DEFAULT 'claude'");
    this.ensureColumn("threads", "codex_session_id", "TEXT");
    this.ensurePinnedSection();
    this.migrateLegacyPinState();
    this.sanitizeLegacyEnumColumns();
  }
  ensurePinnedSection() {
    const now = nowSeconds();
    this.db.prepare(
      `INSERT OR IGNORE INTO thread_sections
         (id, name, appearance_json, created_at, updated_at)
         VALUES (?, ?, NULL, ?, ?)`
    ).run(PINNED_SECTION_ID, PINNED_SECTION_NAME, now, now);
  }
  migrateLegacyPinState() {
    this.db.prepare(
      `UPDATE threads
         SET section_id = ?,
             section_entered_at = COALESCE(section_entered_at, updated_at),
             section_position = COALESCE(section_position, rowid)
         WHERE is_pinned = 1 AND (section_id IS NULL OR section_id = '')`
    ).run(PINNED_SECTION_ID);
    this.db.prepare(
      `UPDATE threads
         SET is_pinned = CASE WHEN section_id = ? THEN 1 ELSE 0 END
         WHERE section_id IS NOT NULL OR is_pinned = 1`
    ).run(PINNED_SECTION_ID);
    this.db.exec(
      `UPDATE threads
       SET section_entered_at = NULL, section_position = NULL
       WHERE section_id IS NULL`
    );
  }
  // Older versions of the adapter stored empty strings (and a snake_case
  // `app_server` source) in columns the Codex App treats as strict enums.
  // Codex App's ts-rs deserializer rejects the invalid values on `thread/list`
  // / `thread/read`, surfacing as the generic "Oops, an error has occurred"
  // toast when reopening a page. Repair the rows in place once on startup so
  // a downgrade-then-upgrade cycle doesn't leave permanent landmines.
  sanitizeLegacyEnumColumns() {
    try {
      this.db.exec(`
        UPDATE threads
        SET thread_source = NULL
        WHERE thread_source IS NOT NULL
          AND thread_source NOT IN ('user', 'subagent', 'memory_consolidation');
        UPDATE threads SET agent_role = NULL WHERE agent_role = '';
        UPDATE threads SET agent_nickname = NULL WHERE agent_nickname = '';
        UPDATE threads SET base_instructions = NULL WHERE base_instructions = '';
        UPDATE threads SET developer_instructions = NULL WHERE developer_instructions = '';
        UPDATE threads SET source = 'appServer' WHERE source = 'app_server';
      `);
    } catch {
    }
  }
  ensureColumn(table, column, definition) {
    const rows = this.db.prepare(`PRAGMA table_info(${table})`).all();
    if (rows.some((row) => String(row.name) === column)) return;
    this.db.exec(`ALTER TABLE ${table} ADD COLUMN ${column} ${definition}`);
  }
  upsertThread(thread) {
    const sectionId = thread.sectionId === void 0 ? thread.isPinned === true ? PINNED_SECTION_ID : null : thread.sectionId;
    const isPinned = sectionId === PINNED_SECTION_ID;
    const sectionEnteredAt = sectionId == null ? null : thread.sectionEnteredAt ?? thread.updatedAt;
    const sectionPosition = sectionId == null ? null : thread.sectionPosition ?? null;
    this.db.prepare(`
        INSERT INTO threads (
          id, session_id, forked_from_id, is_pinned, section_id, section_entered_at, section_position,
          preview, name, archived, cwd, model, reasoning_effort, model_provider, claude_session_id,
          source, created_at, updated_at, status_json,
          approval_policy, sandbox_mode, permission_profile_id, ephemeral, thread_source, agent_role, agent_nickname,
          base_instructions, developer_instructions, personality, runtime_backend, codex_session_id, collaboration_mode
        ) VALUES (
          ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
          ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
        )
        ON CONFLICT(id) DO UPDATE SET
          session_id=excluded.session_id,
          forked_from_id=excluded.forked_from_id,
          is_pinned=excluded.is_pinned,
          section_id=excluded.section_id,
          section_entered_at=excluded.section_entered_at,
          section_position=excluded.section_position,
          preview=excluded.preview,
          name=excluded.name,
          archived=excluded.archived,
          cwd=excluded.cwd,
          model=excluded.model,
          reasoning_effort=excluded.reasoning_effort,
          model_provider=excluded.model_provider,
          claude_session_id=excluded.claude_session_id,
          source=excluded.source,
          updated_at=excluded.updated_at,
          status_json=excluded.status_json,
          approval_policy=excluded.approval_policy,
          sandbox_mode=excluded.sandbox_mode,
          permission_profile_id=excluded.permission_profile_id,
          ephemeral=excluded.ephemeral,
          thread_source=excluded.thread_source,
          agent_role=excluded.agent_role,
          agent_nickname=excluded.agent_nickname,
          base_instructions=excluded.base_instructions,
          developer_instructions=excluded.developer_instructions,
          personality=excluded.personality,
          runtime_backend=excluded.runtime_backend,
          codex_session_id=excluded.codex_session_id,
          collaboration_mode=excluded.collaboration_mode
      `).run(
      thread.id,
      thread.sessionId,
      thread.forkedFromId,
      isPinned ? 1 : 0,
      sectionId,
      sectionEnteredAt,
      sectionPosition,
      thread.preview,
      thread.name,
      thread.archived ? 1 : 0,
      thread.cwd,
      thread.model,
      thread.reasoningEffort,
      thread.modelProvider,
      thread.claudeSessionId,
      thread.source,
      thread.createdAt,
      thread.updatedAt,
      JSON.stringify(thread.status),
      thread.approvalPolicy,
      thread.sandboxMode,
      thread.permissionProfileId ?? null,
      thread.ephemeral ? 1 : 0,
      thread.threadSource,
      thread.agentRole,
      thread.agentNickname,
      thread.baseInstructions,
      thread.developerInstructions,
      thread.personality,
      thread.runtimeBackend,
      thread.codexSessionId,
      thread.collaborationMode ?? "default"
    );
  }
  getThread(id) {
    const row = this.db.prepare("SELECT * FROM threads WHERE id = ?").get(id);
    return row ? this.rowToThread(row) : null;
  }
  listThreads(options = {}) {
    const limit = Math.max(1, Math.min(Number(options.limit ?? 50), 200));
    const archived = options.archived === true ? 1 : 0;
    const sortKey = options.sortKey === "updated_at" || options.sortKey === "recency_at" || options.sortKey === "section_position" ? options.sortKey : "created_at";
    const sortExpression = sortKey === "section_position" ? "COALESCE(t.section_position, t.created_at)" : `t.${sortKey === "created_at" ? "created_at" : "updated_at"}`;
    const sortDirection = options.sortDirection === "asc" ? "ASC" : "DESC";
    const cursor = options.cursor ? Number(options.cursor) : sortDirection === "ASC" ? -1 : Number.MAX_SAFE_INTEGER;
    const where = ["t.archived = ?", `${sortExpression} ${sortDirection === "ASC" ? ">" : "<"} ?`];
    const args = [archived, cursor];
    if (options.isPinned != null) {
      where.push("t.is_pinned = ?");
      args.push(options.isPinned ? 1 : 0);
    }
    if (options.sectionId !== void 0) {
      if (options.sectionId === null) {
        where.push("t.section_id IS NULL");
      } else {
        where.push("t.section_id = ?");
        args.push(options.sectionId);
      }
    }
    const parentThreadId = options.parentThreadId ?? null;
    const ancestorThreadId = options.ancestorThreadId ?? null;
    const sourceKinds = options.sourceKinds ?? [];
    if (parentThreadId != null) {
      where.push("t.forked_from_id = ?");
      args.push(parentThreadId);
    }
    if (ancestorThreadId != null) {
      where.push("t.id IN (SELECT id FROM descendants)");
    }
    if (sourceKinds.length > 0) {
      const predicates = sourceKinds.flatMap((kind) => {
        switch (kind) {
          case "subAgent":
          case "subAgentThreadSpawn":
            return ["(t.thread_source = 'subagent' AND t.forked_from_id IS NOT NULL)"];
          case "subAgentReview":
          case "subAgentCompact":
          case "subAgentOther":
            return [];
          case "user":
            return ["t.thread_source = 'user'"];
          case "memoryConsolidation":
            return ["t.thread_source = 'memory_consolidation'"];
          case "appServer":
          case "cli":
          case "exec":
          case "vscode":
          case "unknown":
            return [`t.source = '${kind}'`];
          default:
            return [];
        }
      });
      where.push(predicates.length > 0 ? `(${predicates.join(" OR ")})` : "1 = 0");
    }
    const hasSubagentSourceFilter = sourceKinds.some(
      (kind) => ["subAgent", "subAgentThreadSpawn"].includes(kind)
    );
    const topologyQuery = parentThreadId != null || ancestorThreadId != null || hasSubagentSourceFilter;
    if (!options.includeEphemeral && !topologyQuery) where.push("t.ephemeral = 0");
    const cwdList = Array.isArray(options.cwd) ? options.cwd : options.cwd ? [options.cwd] : [];
    if (cwdList.length > 0) {
      const placeholders = cwdList.map(() => "?").join(",");
      where.push(`t.cwd IN (${placeholders})`);
      args.push(...cwdList);
    }
    const cte = ancestorThreadId == null ? "" : `WITH RECURSIVE descendants(id) AS (
            SELECT id FROM threads WHERE forked_from_id = ?
            UNION
            SELECT child.id FROM threads child JOIN descendants parent ON child.forked_from_id = parent.id
          )`;
    const queryArgs = ancestorThreadId == null ? args : [ancestorThreadId, ...args];
    const rows = this.db.prepare(
      `${cte} SELECT t.* FROM threads t WHERE ${where.join(" AND ")} ORDER BY ${sortExpression} ${sortDirection}, t.id ${sortDirection} LIMIT ?`
    ).all(...queryArgs, limit);
    return rows.map((row) => this.rowToThread(row));
  }
  listSections(limit = 100, cursor = null) {
    const boundedLimit = Math.max(1, Math.min(Number(limit || 100), 200));
    const offset = cursor == null ? 0 : Math.max(0, Number(cursor) || 0);
    const rows = this.db.prepare(
      `SELECT * FROM thread_sections
         ORDER BY CASE WHEN id = ? THEN 0 ELSE 1 END, created_at ASC, id ASC
         LIMIT ? OFFSET ?`
    ).all(PINNED_SECTION_ID, boundedLimit, offset);
    return rows.map((row) => this.rowToSection(row));
  }
  getSection(id) {
    const row = this.db.prepare("SELECT * FROM thread_sections WHERE id = ?").get(id);
    if (row) return this.rowToSection(row);
    if (id === PINNED_SECTION_ID) {
      return { id: PINNED_SECTION_ID, name: PINNED_SECTION_NAME, appearance: null };
    }
    return null;
  }
  createSection(id, name2, appearance = null) {
    const normalizedName = name2.trim();
    if (!normalizedName) throw new Error("section name must not be empty");
    if (id === PINNED_SECTION_ID) throw new Error("the pinned section is reserved");
    const now = nowSeconds();
    this.db.prepare(
      `INSERT INTO thread_sections
         (id, name, appearance_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)`
    ).run(id, normalizedName, appearance == null ? null : JSON.stringify(appearance), now, now);
    return this.getSection(id);
  }
  updateSection(id, name2, appearance = null) {
    if (!this.getSection(id)) throw new Error(`unknown section: ${id}`);
    const normalizedName = id === PINNED_SECTION_ID ? PINNED_SECTION_NAME : name2.trim();
    if (!normalizedName) throw new Error("section name must not be empty");
    this.db.prepare(
      `UPDATE thread_sections
         SET name = ?, appearance_json = ?, updated_at = ?
         WHERE id = ?`
    ).run(normalizedName, appearance == null ? null : JSON.stringify(appearance), nowSeconds(), id);
    return this.getSection(id);
  }
  deleteSection(id) {
    if (id === PINNED_SECTION_ID) throw new Error("the pinned section is reserved");
    if (!this.getSection(id)) throw new Error(`unknown section: ${id}`);
    this.db.exec("BEGIN IMMEDIATE");
    try {
      this.db.prepare(
        `UPDATE threads
           SET section_id = NULL,
               section_entered_at = NULL,
               section_position = NULL,
               is_pinned = 0
           WHERE section_id = ?`
      ).run(id);
      this.db.prepare("DELETE FROM thread_sections WHERE id = ?").run(id);
      this.db.exec("COMMIT");
    } catch (error) {
      try {
        this.db.exec("ROLLBACK");
      } catch {
      }
      throw error;
    }
  }
  moveThreadToSection(threadId, sectionId, beforeThreadId = null) {
    const thread = this.getThread(threadId);
    if (!thread) throw new Error(`unknown thread: ${threadId}`);
    if (sectionId != null && !this.getSection(sectionId))
      throw new Error(`unknown section: ${sectionId}`);
    const oldSectionId = thread.sectionId === void 0 ? thread.isPinned === true ? PINNED_SECTION_ID : null : thread.sectionId ?? null;
    const oldIds = this.sectionThreadIds(oldSectionId, threadId);
    const targetIds = sectionId == null ? [] : this.sectionThreadIds(sectionId, threadId);
    let targetPosition = null;
    if (sectionId != null) {
      const beforeIndex = beforeThreadId == null ? -1 : targetIds.indexOf(beforeThreadId);
      targetPosition = beforeIndex >= 0 ? beforeIndex : targetIds.length;
      targetIds.splice(targetPosition, 0, threadId);
    }
    const enteredAt = sectionId == null ? null : oldSectionId === sectionId ? thread.sectionEnteredAt ?? nowSeconds() : nowSeconds();
    this.db.exec("BEGIN IMMEDIATE");
    try {
      if (oldSectionId !== sectionId && oldSectionId != null) this.renumberSection(oldIds);
      if (sectionId != null) this.renumberSection(targetIds);
      this.db.prepare(
        `UPDATE threads
           SET section_id = ?,
               section_entered_at = ?,
               section_position = ?,
               is_pinned = ?
           WHERE id = ?`
      ).run(
        sectionId,
        enteredAt,
        targetPosition,
        sectionId === PINNED_SECTION_ID ? 1 : 0,
        threadId
      );
      this.db.exec("COMMIT");
    } catch (error) {
      try {
        this.db.exec("ROLLBACK");
      } catch {
      }
      throw error;
    }
    const moved = this.getThread(threadId);
    if (!moved) throw new Error(`unknown thread: ${threadId}`);
    return moved;
  }
  sectionThreadIds(sectionId, excludeThreadId) {
    const rows = sectionId == null ? this.db.prepare(
      `SELECT id FROM threads
               WHERE section_id IS NULL AND id <> ?
               ORDER BY COALESCE(section_position, created_at), updated_at, id`
    ).all(excludeThreadId) : this.db.prepare(
      `SELECT id FROM threads
               WHERE section_id = ? AND id <> ?
               ORDER BY COALESCE(section_position, created_at), updated_at, id`
    ).all(sectionId, excludeThreadId);
    return rows.map((row) => String(row.id));
  }
  renumberSection(threadIds) {
    const update = this.db.prepare("UPDATE threads SET section_position = ? WHERE id = ?");
    for (const [position, threadId] of threadIds.entries()) update.run(position, threadId);
  }
  updateThreadStatus(threadId, status) {
    this.db.prepare("UPDATE threads SET status_json = ?, updated_at = ? WHERE id = ?").run(JSON.stringify(status), nowSeconds(), threadId);
  }
  updateThreadName(threadId, name2) {
    this.db.prepare("UPDATE threads SET name = ?, updated_at = ? WHERE id = ?").run(name2, nowSeconds(), threadId);
  }
  updateClaudeSessionId(threadId, claudeSessionId) {
    this.db.prepare("UPDATE threads SET claude_session_id = ?, updated_at = ? WHERE id = ?").run(claudeSessionId, nowSeconds(), threadId);
  }
  // Stored separately from Claude's session id so a thread that was
  // codex-backed can be later inspected without confusing its provenance,
  // and so a codex thread's resume id never collides with a claude session id.
  updateCodexSessionId(threadId, codexSessionId) {
    this.db.prepare("UPDATE threads SET codex_session_id = ?, updated_at = ? WHERE id = ?").run(codexSessionId, nowSeconds(), threadId);
  }
  setArchived(threadId, archived) {
    this.db.prepare("UPDATE threads SET archived = ?, updated_at = ? WHERE id = ?").run(archived ? 1 : 0, nowSeconds(), threadId);
  }
  upsertTurn(turn) {
    this.db.prepare(`
        INSERT INTO turns (id, thread_id, status, started_at, completed_at, duration_ms, items_json, diff, error_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          status=excluded.status,
          started_at=excluded.started_at,
          completed_at=excluded.completed_at,
          duration_ms=excluded.duration_ms,
          items_json=excluded.items_json,
          diff=excluded.diff,
          error_json=excluded.error_json
      `).run(
      turn.id,
      turn.threadId,
      turn.status,
      turn.startedAt,
      turn.completedAt,
      turn.durationMs,
      JSON.stringify(turn.items),
      turn.diff,
      turn.error == null ? null : JSON.stringify(turn.error)
    );
  }
  getTurn(id) {
    const row = this.db.prepare("SELECT * FROM turns WHERE id = ?").get(id);
    return row ? this.rowToTurn(row) : null;
  }
  listTurns(threadId) {
    const rows = this.db.prepare("SELECT * FROM turns WHERE thread_id = ? ORDER BY started_at ASC").all(threadId);
    return rows.map((row) => this.rowToTurn(row));
  }
  // Used by thread/rollback to drop the N most recent turns from a thread's
  // history. Codex App's rewind UI calls this when the user wants to redo a
  // turn — without it our handler used to no-op and the App's "Rewind" button
  // appeared dead. Returns the number of turns actually removed.
  deleteRecentTurns(threadId, numTurns) {
    if (numTurns <= 0) return 0;
    const ids = this.db.prepare("SELECT id FROM turns WHERE thread_id = ? ORDER BY started_at DESC LIMIT ?").all(threadId, numTurns);
    if (ids.length === 0) return 0;
    const stmt = this.db.prepare("DELETE FROM turns WHERE id = ?");
    for (const row of ids) stmt.run(row.id);
    return ids.length;
  }
  appendItem(turnId, item) {
    const turn = this.getTurn(turnId);
    if (!turn) return null;
    turn.items.push(jsonClone(item));
    this.upsertTurn(turn);
    return turn;
  }
  updateItem(turnId, itemId, updater) {
    const turn = this.getTurn(turnId);
    if (!turn) return null;
    turn.items = turn.items.map((item) => item.id === itemId ? updater(jsonClone(item)) : item);
    this.upsertTurn(turn);
    return turn;
  }
  updateItemAndMoveToEnd(turnId, itemId, updater) {
    const turn = this.getTurn(turnId);
    if (!turn) return null;
    const index = turn.items.findIndex((item) => item.id === itemId);
    if (index < 0) return turn;
    const current = turn.items[index];
    if (!current) return turn;
    const updated = updater(jsonClone(current));
    turn.items = [...turn.items.slice(0, index), ...turn.items.slice(index + 1), updated];
    this.upsertTurn(turn);
    return turn;
  }
  completeTurn(turnId, status, error = null) {
    const turn = this.getTurn(turnId);
    if (!turn) return null;
    const completedAt = nowSeconds();
    turn.status = status;
    turn.completedAt = completedAt;
    turn.durationMs = turn.startedAt == null ? null : Math.max(0, (completedAt - turn.startedAt) * 1e3);
    turn.error = error;
    this.upsertTurn(turn);
    return turn;
  }
  recoverStaleInProgressTurns(message = "server restarted before completing turn") {
    const rows = this.db.prepare("SELECT id, thread_id, started_at, items_json FROM turns WHERE status = ?").all("inProgress");
    const completedAt = nowSeconds();
    const errorJson = JSON.stringify({ message });
    const updateTurn = this.db.prepare(`
      UPDATE turns
      SET status = ?, completed_at = ?, duration_ms = ?, error_json = ?, items_json = ?
      WHERE id = ?
    `);
    const updateThread = this.db.prepare(
      "UPDATE threads SET status_json = ?, updated_at = ? WHERE id = ?"
    );
    const seenThreads = /* @__PURE__ */ new Set();
    let recoveredCount = 0;
    for (const row of rows) {
      const startedAt = row.started_at == null ? null : Number(row.started_at);
      const durationMs = startedAt == null ? null : Math.max(0, (completedAt - startedAt) * 1e3);
      updateTurn.run(
        "interrupted",
        completedAt,
        durationMs,
        errorJson,
        this.terminalizeStaleItems(row.items_json, message),
        row.id
      );
      seenThreads.add(String(row.thread_id));
      recoveredCount += 1;
    }
    for (const threadId of seenThreads) {
      updateThread.run(JSON.stringify({ type: "idle" }), completedAt, threadId);
    }
    const terminalRows = this.db.prepare("SELECT id, thread_id, status, items_json FROM turns WHERE status <> ?").all("inProgress");
    const updateItems = this.db.prepare("UPDATE turns SET items_json = ? WHERE id = ?");
    const repairThreadIds = /* @__PURE__ */ new Set();
    for (const row of terminalRows) {
      const itemsJson = this.terminalizeStaleItems(
        row.items_json,
        message,
        row.status !== "completed",
        row.status === "completed"
      );
      if (itemsJson === row.items_json) continue;
      updateItems.run(itemsJson, row.id);
      repairThreadIds.add(String(row.thread_id));
      recoveredCount += 1;
    }
    const reconcileThread = this.db.prepare(
      `UPDATE threads SET status_json = ?, updated_at = ?
       WHERE id = ?
         AND NOT EXISTS (SELECT 1 FROM turns WHERE thread_id = threads.id AND status = ?)`
    );
    for (const threadId of repairThreadIds) {
      reconcileThread.run(JSON.stringify({ type: "idle" }), completedAt, threadId, "inProgress");
    }
    const reconcileTerminalActiveThreads = this.db.prepare(
      `UPDATE threads SET status_json = ?, updated_at = ?
       WHERE json_extract(status_json, '$.type') = 'active'
         AND NOT EXISTS (SELECT 1 FROM turns WHERE thread_id = threads.id AND status = ?)`
    );
    const reconciled = reconcileTerminalActiveThreads.run(
      JSON.stringify({ type: "idle" }),
      completedAt,
      "inProgress"
    );
    recoveredCount += Number(reconciled.changes ?? 0);
    return recoveredCount;
  }
  // A recovered turn is also replayed from its persisted item list. Clear any
  // item-level liveness markers so the App's spinner projection cannot keep a
  // child agent in `working` after the turn/thread has been terminalized.
  terminalizeStaleItems(itemsJson, message, terminalizeActivity = true, stripCompletedActivity = false) {
    let parsed;
    try {
      parsed = JSON.parse(itemsJson);
    } catch {
      return "[]";
    }
    if (!Array.isArray(parsed)) return "[]";
    const items = parsed.map((raw) => {
      if (raw == null || typeof raw !== "object" || Array.isArray(raw)) return raw;
      const item = raw;
      if (stripCompletedActivity && item.type === "subAgentActivity") return null;
      if (terminalizeActivity && item.type === "subAgentActivity" && (item.kind === "started" || item.kind === "interacted")) {
        return { ...item, kind: "interrupted" };
      }
      if (item.type === "subAgentActivity" && item.kind === "completed") {
        return { ...item, kind: "interrupted" };
      }
      if (item.type === "collabAgentToolCall") {
        const agentsStates = item.agentsStates;
        const nextStates = agentsStates && typeof agentsStates === "object" && !Array.isArray(agentsStates) ? Object.fromEntries(
          Object.entries(agentsStates).map(
            ([threadId, rawState]) => {
              if (rawState == null || typeof rawState !== "object" || Array.isArray(rawState))
                return [threadId, rawState];
              const state = rawState;
              if (state.status !== "pendingInit" && state.status !== "running")
                return [threadId, state];
              return [
                threadId,
                { ...state, status: "errored", message: state.message ?? message }
              ];
            }
          )
        ) : agentsStates;
        return {
          ...item,
          ...item.status === "inProgress" ? { status: "failed" } : {},
          agentsStates: nextStates
        };
      }
      if (item.status === "inProgress") return { ...item, status: "failed" };
      return item;
    }).filter((item) => item != null);
    return JSON.stringify(items);
  }
  updateTurnDiff(turnId, diff) {
    const turn = this.getTurn(turnId);
    if (!turn) return null;
    turn.diff = diff;
    this.upsertTurn(turn);
    return turn;
  }
  close() {
    this.db.close();
  }
  rowToThread(row) {
    return {
      collaborationMode: row.collaboration_mode === "plan" ? "plan" : "default",
      id: String(row.id),
      sessionId: String(row.session_id),
      forkedFromId: row.forked_from_id == null ? null : String(row.forked_from_id),
      isPinned: String(row.section_id ?? "") === PINNED_SECTION_ID || Number(row.is_pinned ?? 0) === 1,
      sectionId: row.section_id == null ? null : String(row.section_id),
      sectionEnteredAt: row.section_entered_at == null ? null : Number(row.section_entered_at),
      sectionPosition: row.section_position == null ? null : Number(row.section_position),
      preview: String(row.preview ?? ""),
      name: row.name == null ? null : String(row.name),
      archived: Number(row.archived) === 1,
      cwd: String(row.cwd),
      model: String(row.model),
      reasoningEffort: row.reasoning_effort == null ? null : String(row.reasoning_effort),
      modelProvider: String(row.model_provider),
      claudeSessionId: row.claude_session_id == null ? null : String(row.claude_session_id),
      source: String(row.source),
      createdAt: Number(row.created_at),
      updatedAt: Number(row.updated_at),
      status: JSON.parse(String(row.status_json)),
      approvalPolicy: row.approval_policy == null ? null : String(row.approval_policy),
      sandboxMode: row.sandbox_mode == null ? null : String(row.sandbox_mode),
      permissionProfileId: row.permission_profile_id == null ? null : String(row.permission_profile_id),
      ephemeral: Number(row.ephemeral ?? 0) === 1,
      threadSource: row.thread_source == null ? null : String(row.thread_source),
      agentRole: row.agent_role == null ? null : String(row.agent_role),
      agentNickname: row.agent_nickname == null ? null : String(row.agent_nickname),
      baseInstructions: row.base_instructions == null ? null : String(row.base_instructions),
      developerInstructions: row.developer_instructions == null ? null : String(row.developer_instructions),
      personality: row.personality == null ? null : String(row.personality),
      runtimeBackend: row.runtime_backend === "codex" ? "codex" : "claude",
      codexSessionId: row.codex_session_id == null ? null : String(row.codex_session_id)
    };
  }
  rowToSection(row) {
    let appearance = null;
    if (row.appearance_json != null) {
      try {
        const parsed = JSON.parse(String(row.appearance_json));
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          const value = parsed;
          appearance = {
            color: typeof value.color === "string" ? value.color : null,
            icon: typeof value.icon === "string" ? value.icon : null
          };
        }
      } catch {
      }
    }
    return {
      id: String(row.id),
      name: String(row.name ?? ""),
      appearance
    };
  }
  rowToTurn(row) {
    return {
      id: String(row.id),
      threadId: String(row.thread_id),
      status: String(row.status),
      startedAt: row.started_at == null ? null : Number(row.started_at),
      completedAt: row.completed_at == null ? null : Number(row.completed_at),
      durationMs: row.duration_ms == null ? null : Number(row.duration_ms),
      items: JSON.parse(String(row.items_json)),
      diff: String(row.diff ?? ""),
      error: row.error_json == null ? null : JSON.parse(String(row.error_json))
    };
  }
};

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/workflow-command.mts
var WORKFLOWS_COMMAND = /^\s*\/workflows(?=$|\s)([\s\S]*)$/;
function parseWorkflowCommand(prompt) {
  const match = prompt.match(WORKFLOWS_COMMAND);
  if (!match) return null;
  const task = (match[1] ?? "").trim();
  return task ? { type: "run", prompt: task } : { type: "list" };
}
function workflowRuntimePrompt(prompt) {
  const command = parseWorkflowCommand(prompt);
  if (command?.type !== "run") return prompt;
  return [
    `ultracode: ${command.prompt}`,
    "",
    "Keep the launched workflow attached to this turn: wait for every workflow task to finish before returning the final response."
  ].join("\n");
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/worktree.mts
import { execFileSync } from "node:child_process";
import { mkdirSync as mkdirSync3, realpathSync } from "node:fs";
import { join as join7, resolve as resolve3 } from "node:path";
var labelHashLength = 16;
var labelPrefixLength = 48;
function maybeCreateThreadWorktree(threadId, cwd) {
  if (process.env.CLAUDE_CODEX_AUTO_WORKTREE !== "1") {
    return { cwd, created: false };
  }
  const root = resolve3(process.env.CLAUDE_CODEX_WORKTREE_ROOT || join7(adapterHome(), "worktrees"));
  mkdirSync3(root, { recursive: true });
  const label = worktreeLabel(threadId);
  const worktreePath = join7(root, label);
  if (isExistingWorktree(worktreePath)) return { cwd: worktreePath, created: false };
  try {
    execFileSync("git", ["rev-parse", "--show-toplevel"], {
      cwd,
      stdio: ["ignore", "ignore", "ignore"]
    });
    execFileSync("git", ["worktree", "add", "-b", `claude-codex/${label}`, worktreePath, "HEAD"], {
      cwd,
      stdio: ["ignore", "ignore", "pipe"]
    });
    return { cwd: worktreePath, created: true };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(
      `[claude-codex-adapter] failed to create worktree for ${threadId}: ${message}
`
    );
    return { cwd, created: false };
  }
}
function worktreeLabel(threadId) {
  const readable = threadId.replace(/[^A-Za-z0-9-]+/g, "-").replace(/-+/g, "-").replace(/^-|-$/g, "").slice(0, labelPrefixLength);
  const prefix = readable.length > 0 ? readable : "thread";
  return `${prefix}-${stableHash(threadId).slice(0, labelHashLength)}`;
}
function isExistingWorktree(path) {
  try {
    const topLevel = execFileSync("git", ["rev-parse", "--show-toplevel"], {
      cwd: path,
      encoding: "utf8"
    }).trim();
    return realpathSync(topLevel) === realpathSync(path);
  } catch (error) {
    if (error instanceof Error) return false;
    throw error;
  }
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/server.mts
var execFileAsync2 = promisify2(execFile2);
var DEFAULT_SUBAGENT_WATCHDOG_MS = 30 * 60 * 1e3;
var MIN_SUBAGENT_WATCHDOG_MS = 100;
function isWorkflowToolName(value) {
  const name2 = value.trim().toLowerCase();
  return name2 === "workflow" || name2 === "workflows";
}
function subagentWatchdogTimeoutMs(env = process.env) {
  const raw = env.CLAUDE_CODEX_SUBAGENT_TIMEOUT_MS?.trim();
  if (!raw) return DEFAULT_SUBAGENT_WATCHDOG_MS;
  const parsed = Number(raw);
  if (!Number.isFinite(parsed)) return DEFAULT_SUBAGENT_WATCHDOG_MS;
  if (parsed === 0) return 0;
  if (parsed < 0) return DEFAULT_SUBAGENT_WATCHDOG_MS;
  return Math.max(MIN_SUBAGENT_WATCHDOG_MS, Math.floor(parsed));
}
function turnItemsView(value) {
  if (value === "full" || value === "summary" || value === "notLoaded") return value;
  return "summary";
}
var CodexClaudeAppServer = class {
  pendingServerRequests = /* @__PURE__ */ new Map();
  activePeerByThread = /* @__PURE__ */ new Map();
  peerFeatures = /* @__PURE__ */ new WeakMap();
  activeTurnByThread = /* @__PURE__ */ new Map();
  subagentStateByTurn = /* @__PURE__ */ new Map();
  fuzzySessions = /* @__PURE__ */ new Map();
  commandSessionAllow = /* @__PURE__ */ new Map();
  commandProcesses = /* @__PURE__ */ new Map();
  processHandles = /* @__PURE__ */ new Map();
  fsWatchers = /* @__PURE__ */ new Map();
  goals = /* @__PURE__ */ new Map();
  elicitationCounts = /* @__PURE__ */ new Map();
  tokenUsageByThread = /* @__PURE__ */ new Map();
  rateLimits = new RateLimitStore();
  rateLimitRefresh = null;
  configModel = defaultSelectableModelId();
  configReasoningEffort = normalizeCodexReasoningEffort(process.env.CLAUDE_CODEX_DEFAULT_EFFORT) ?? "medium";
  // Catch-all for arbitrary keys the App's settings sheet writes (approval
  // policy, sandbox preference, instructions toggles, etc.). We don't apply
  // them to typed runtime state, but we round-trip them through config/read
  // so the user's settings survive a daemon restart instead of resetting on
  // every reconnect.
  configOverrides = {};
  configPath = join8(adapterHome(), "config.json");
  idleCheckHandler = null;
  stopped = false;
  store;
  runtime;
  projects;
  constructor(store, runtime) {
    this.store = store;
    this.runtime = runtime;
    this.projects = new ProjectStore((id) => this.store.getThread(id));
    this.loadPersistedConfig();
  }
  async handle(peer, message) {
    if ("method" in message && message.method) {
      debugLog("rpc.request", {
        peerId: peer.id,
        id: "id" in message ? message.id : null,
        method: message.method,
        params: summarizeRpcParams(message.method, message.params)
      });
      if ("id" in message) {
        await this.handleRequest(peer, message);
      } else {
        await this.handleNotification(peer, message);
      }
      return;
    }
    if ("id" in message) {
      debugLog("rpc.responseFromClient", {
        peerId: peer.id,
        id: message.id,
        hasError: Boolean(message.error)
      });
      this.resolveServerRequest(message);
    }
  }
  closePeer(peer) {
    debugLog("peer.close", { peerId: peer.id });
    for (const [threadId, activePeer] of this.activePeerByThread.entries()) {
      if (activePeer.id === peer.id) this.activePeerByThread.delete(threadId);
    }
  }
  async stop() {
    if (this.stopped) return;
    this.stopped = true;
    this.finalizeActiveSubagentsForShutdown("server stopped");
    this.completeActiveTurns("interrupted", { message: "server stopped" });
    await this.runtime.stop();
    this.store.close();
  }
  hasActiveTurns() {
    return this.activeTurnByThread.size > 0;
  }
  setIdleCheckHandler(handler) {
    this.idleCheckHandler = handler;
  }
  async handleNotification(_peer, _message) {
  }
  async handleRequest(peer, request) {
    try {
      const result = await this.dispatch(peer, request.method, request.params ?? {});
      debugLog("rpc.response", {
        peerId: peer.id,
        id: request.id,
        method: request.method,
        ok: true
      });
      this.sendResponse(peer, request.id, result);
    } catch (error) {
      debugLog("rpc.response", {
        peerId: peer.id,
        id: request.id,
        method: request.method,
        ok: false,
        error: error instanceof Error ? error.message : String(error),
        stack: error instanceof Error ? error.stack : null
      });
      this.sendResponse(peer, request.id, void 0, {
        code: -32e3,
        message: error instanceof Error ? error.message : String(error)
      });
    }
  }
  async dispatch(peer, method, params) {
    switch (method) {
      case "initialize": {
        const initParams = asRecord3(params);
        const clientInfo = asRecord3(initParams.clientInfo);
        const capabilities = asRecord3(initParams.capabilities);
        const declaredActivityKinds = Array.isArray(capabilities.subAgentActivityKinds) ? capabilities.subAgentActivityKinds : [];
        this.peerFeatures.set(peer, {
          supportsCompletedSubagentActivity: capabilities.subAgentActivityCompleted === true || declaredActivityKinds.includes("completed") || process.env.CLAUDE_CODEX_SUBAGENT_COMPLETED === "1"
        });
        queueMicrotask(() => {
          this.notify(peer, {
            method: "account/updated",
            params: { authMode: "apikey", planType: null }
          });
          for (const status of readMcpConfig().startupStatuses) {
            this.notify(peer, {
              method: "mcpServer/startupStatus/updated",
              params: { name: status.name, status: status.status, error: status.error ?? null }
            });
          }
        });
        return {
          userAgent: codexUserAgent(
            stringOr2(clientInfo.name, "codex-app"),
            stringOr2(clientInfo.version, "unknown")
          ),
          codexHome: codexHome(),
          platformFamily: platformFamily(),
          platformOs: platformOs()
        };
      }
      case "project/list":
        return this.projects.list(asRecord3(params));
      case "project/read":
        return { project: this.projects.read(asRecord3(params).projectId) };
      case "project/create": {
        const project = this.projects.create(asRecord3(params));
        this.notify(peer, { method: "project/changed", params: { projectId: project.id, changeType: "created" } });
        return { project };
      }
      case "project/update": {
        const project = this.projects.update(asRecord3(params));
        this.notify(peer, { method: "project/changed", params: { projectId: project.id, changeType: "updated" } });
        return { project };
      }
      case "project/delete": {
        const projectId = asRecord3(params).projectId;
        this.projects.delete(projectId);
        this.notify(peer, { method: "project/changed", params: { projectId, changeType: "deleted" } });
        return {};
      }
      case "thread/start":
        return this.threadStart(peer, asRecord3(params));
      case "thread/resume":
        return this.threadResume(peer, asRecord3(params));
      case "thread/fork":
        return this.threadFork(peer, asRecord3(params));
      case "thread/list":
        return this.threadList(asRecord3(params));
      case "thread/read":
        return this.threadRead(asRecord3(params));
      case "thread/turns/list":
        return this.threadTurnsList(asRecord3(params));
      case "thread/items/list":
        return this.threadItemsList(asRecord3(params));
      case "thread/turns/items/list":
        return this.threadTurnItemsList(asRecord3(params));
      case "thread/name/set":
        return this.threadNameSet(asRecord3(params));
      case "thread/archive":
        return this.threadArchive(asRecord3(params), true);
      case "thread/unsubscribe":
        return this.threadUnsubscribe(peer, asRecord3(params));
      case "thread/increment_elicitation":
        return this.threadAdjustElicitation(asRecord3(params), 1);
      case "thread/decrement_elicitation":
        return this.threadAdjustElicitation(asRecord3(params), -1);
      case "thread/goal/set":
        return this.threadGoalSet(asRecord3(params));
      case "thread/goal/get":
        return this.threadGoalGet(asRecord3(params));
      case "thread/goal/clear":
        return this.threadGoalClear(asRecord3(params));
      case "thread/metadata/update":
        return this.threadMetadataUpdate(asRecord3(params));
      case "thread/section/move":
        return this.threadSectionMove(asRecord3(params));
      case "threadSection/list":
        return this.threadSectionList(asRecord3(params));
      case "threadSection/create":
        return this.threadSectionCreate(asRecord3(params));
      case "threadSection/update":
        return this.threadSectionUpdate(asRecord3(params));
      case "threadSection/delete":
        return this.threadSectionDelete(asRecord3(params));
      case "thread/settings/update":
        return this.threadSettingsUpdate(peer, asRecord3(params));
      // Intentional no-ops: Claude Code has no equivalent concept, so the
      // adapter acknowledges the call without side effects rather than failing
      // the RPC (which would break the Codex App connection).
      case "thread/memoryMode/set":
      case "memory/reset":
        return {};
      case "thread/unarchive":
        return this.threadArchive(asRecord3(params), false);
      case "thread/compact/start":
        return this.threadCompactStart(peer, asRecord3(params));
      case "thread/shellCommand":
        return this.threadShellCommand(peer, asRecord3(params));
      case "thread/approveGuardianDeniedAction": {
        const evt = asRecord3(params).event;
        debugLog("thread.approveGuardianDeniedAction", {
          threadId: stringOr2(asRecord3(params).threadId, ""),
          eventType: evt && typeof evt === "object" ? evt.type ?? null : null
        });
        return {};
      }
      case "thread/backgroundTerminals/clean":
        return this.threadBackgroundTerminalsClean(asRecord3(params));
      case "thread/rollback":
        return this.threadRollback(asRecord3(params));
      case "thread/loaded/list":
        return this.threadLoadedList(asRecord3(params));
      case "thread/inject_items":
        return this.threadInjectItems(peer, asRecord3(params));
      case "turn/start":
        return this.turnStart(peer, asRecord3(params));
      case "turn/steer":
        return this.turnSteer(peer, asRecord3(params));
      case "turn/interrupt":
        return this.turnInterrupt(peer, asRecord3(params));
      // Realtime voice is unsupported: Claude Code has no realtime audio
      // channel. These ack so the App's capability probe does not error; a
      // real session would need a separate audio backend.
      case "thread/realtime/start":
      case "thread/realtime/appendAudio":
      case "thread/realtime/appendText":
      case "thread/realtime/stop":
        return {};
      case "thread/realtime/listVoices":
        return { voices: { v1: [], v2: [], defaultV1: null, defaultV2: null } };
      case "review/start":
        return this.reviewStart(peer, asRecord3(params));
      case "config/read":
        return this.configRead();
      case "configRequirements/read":
        return { requirements: null };
      case "model/list":
        return this.modelList();
      case "modelProvider/capabilities/read":
        return {
          namespaceTools: true,
          imageGeneration: false,
          // Claude Code SDK ships a WebSearch tool. Default to advertising it
          // so Codex App shows the search affordance; CLAUDE_CODEX_WEBSEARCH=0
          // turns it off for environments where the tool is rate-limited.
          webSearch: process.env.CLAUDE_CODEX_WEBSEARCH !== "0"
        };
      case "experimentalFeature/list":
        return { data: [], nextCursor: null };
      case "permissionProfile/list":
        return permissionProfileList(asRecord3(params));
      case "experimentalFeature/enablement/set":
        return { enablement: asRecord3(asRecord3(params).enablement) };
      case "collaborationMode/list":
        return { data: [
          { name: "Default", mode: "default", model: this.configModel, reasoning_effort: this.configReasoningEffort },
          { name: "Plan", mode: "plan", model: this.configModel, reasoning_effort: this.configReasoningEffort }
        ] };
      case "mock/experimentalMethod":
        return {
          echoed: typeof asRecord3(params).value === "string" ? asRecord3(params).value : null
        };
      case "skills/list":
        return { data: listClaudeSkills(asRecord3(params)) };
      case "hooks/list":
        return { data: listClaudeHooks(asRecord3(params)) };
      case "marketplace/add":
        return this.marketplaceAdd(asRecord3(params));
      case "marketplace/remove":
        return this.marketplaceRemove(asRecord3(params));
      case "marketplace/upgrade":
        return this.marketplaceUpgrade(asRecord3(params));
      case "plugin/list":
        return { marketplaces: [], marketplaceLoadErrors: [], featuredPluginIds: [] };
      case "plugin/read":
        return this.pluginRead(asRecord3(params));
      case "plugin/skill/read":
        return { contents: null };
      case "plugin/share/save":
        return this.pluginShareSave(asRecord3(params));
      case "plugin/share/updateTargets":
        return this.pluginShareUpdateTargets(asRecord3(params));
      case "plugin/share/delete":
      case "plugin/uninstall":
        return {};
      case "plugin/install":
        return { authPolicy: "ON_USE", appsNeedingAuth: [] };
      case "skills/config/write":
        return { effectiveEnabled: asRecord3(params).enabled === true };
      case "plugin/share/list":
        return { data: [] };
      // Stub the three RPC methods Codex App may call but our dispatcher
      // previously threw "method not implemented" on. Stubs return the
      // schema-correct empty shape so the App's call sites don't surface an
      // RPC error toast.
      case "plugin/share/checkout":
        return {};
      case "environment/add":
        return {};
      case "attestation/generate":
        return { attestation: "" };
      case "app/list":
        return { data: [], nextCursor: null };
      case "mcpServer/oauth/login":
        return {
          authorizationUrl: `https://localhost.invalid/claude-codex/mcp-oauth/${encodeURIComponent(stringOr2(asRecord3(params).name, "server"))}`
        };
      case "config/mcpServer/reload":
        return {};
      case "mcpServerStatus/list":
        return listMcpServerStatuses().then((data) => ({ data, nextCursor: null }));
      case "mcpServer/resource/read":
        return readMcpResource(
          stringOr2(asRecord3(params).server, ""),
          stringOr2(asRecord3(params).uri, "")
        );
      case "mcpServer/tool/call":
        return callMcpTool(
          stringOr2(asRecord3(params).server, ""),
          stringOr2(asRecord3(params).tool, ""),
          asRecord3(params).arguments ?? {}
        );
      case "windowsSandbox/setupStart":
        return { started: false };
      case "windowsSandbox/readiness":
        return { status: "notConfigured" };
      case "account/login/start":
        return { type: "apiKey" };
      case "account/login/cancel":
        return { status: "notFound" };
      case "account/logout":
        return {};
      case "account/sendAddCreditsNudgeEmail":
        return { status: "cooldown_active" };
      case "feedback/upload":
        return { threadId: stringOr2(asRecord3(params).threadId, "") };
      case "account/read":
        return { account: { type: "amazonBedrock" }, requiresOpenaiAuth: false };
      case "account/rateLimits/read":
        return this.accountRateLimits();
      case "fs/readFile":
        return this.fsReadFile(asRecord3(params));
      case "fs/readDirectory":
        return this.fsReadDirectory(asRecord3(params));
      case "fs/getMetadata":
        return this.fsGetMetadata(asRecord3(params));
      case "fs/writeFile":
        return this.fsWriteFile(asRecord3(params));
      case "fs/createDirectory":
        return this.fsCreateDirectory(asRecord3(params));
      case "fs/remove":
        return this.fsRemove(asRecord3(params));
      case "fs/copy":
        return this.fsCopy(asRecord3(params));
      case "fs/watch":
        return this.fsWatch(peer, asRecord3(params));
      case "fs/unwatch":
        return this.fsUnwatch(asRecord3(params));
      case "command/exec":
        return this.commandExec(peer, asRecord3(params));
      case "command/exec/write":
        return this.commandExecWrite(asRecord3(params));
      case "command/exec/terminate":
        return this.commandExecTerminate(asRecord3(params));
      case "command/exec/resize":
        return {};
      case "process/spawn":
        return this.processSpawn(peer, asRecord3(params));
      case "process/writeStdin":
        return this.processWriteStdin(asRecord3(params));
      case "process/kill":
        return this.processKill(asRecord3(params));
      case "process/resizePty":
        return this.processResizePty(asRecord3(params));
      case "externalAgentConfig/detect":
        return { items: [] };
      case "externalAgentConfig/import":
        return {};
      case "config/value/write":
      case "config/batchWrite":
        return this.configWriteResponse(asRecord3(params));
      case "getConversationSummary":
        return this.getConversationSummary(asRecord3(params));
      case "gitDiffToRemote":
        return this.gitDiffToRemote(asRecord3(params));
      case "getAuthStatus":
        return { authMethod: null, authToken: null, requiresOpenaiAuth: false };
      case "fuzzyFileSearch":
        return this.fuzzyFileSearch(asRecord3(params));
      case "fuzzyFileSearch/sessionStart":
        return this.fuzzySessionStart(asRecord3(params));
      case "fuzzyFileSearch/sessionUpdate":
        return this.fuzzySessionUpdate(peer, asRecord3(params));
      case "fuzzyFileSearch/sessionStop":
        return this.fuzzySessionStop(peer, asRecord3(params));
      default:
        throw new Error(`method not implemented: ${method}`);
    }
  }
  threadStart(peer, params) {
    const id = newId();
    const now = nowSeconds();
    const project = params.projectId == null ? null : this.projects.read(params.projectId);
    const requestedCwd = stringOr2(params.cwd, project?.roots[0]?.path ?? process.cwd());
    const cwd = maybeCreateThreadWorktree(id, requestedCwd).cwd;
    const model = modelFromParams(params, this.configModel);
    if (process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY === "1" && isCodexOpenAiModel(model))
      throw new Error("Codex proxy models are disabled for this Claude adapter");
    const reasoningEffort = reasoningEffortFromParams(params, this.configReasoningEffort);
    const permissionProfileId = permissionProfileIdFromParams(params);
    const permissionProfile = permissionProfilePolicy(permissionProfileId);
    const selectedProviderLoop = resolveProviderLoopSelection(this.providerLoopSelectionInput());
    const isTitleOrHelper = params.ephemeral === true || params.threadSource === "title_generation" || params.threadSource === "memory_consolidation";
    const thread = {
      id,
      sessionId: id,
      forkedFromId: null,
      isPinned: false,
      sectionId: null,
      sectionEnteredAt: null,
      sectionPosition: null,
      preview: "",
      name: null,
      archived: false,
      cwd,
      model,
      reasoningEffort,
      modelProvider: "claude-code",
      claudeSessionId: null,
      // v2 SessionSource is camelCase; the old `app_server` falls through to
      // `unknown` on the App side, hiding the source in the thread sidebar.
      source: "appServer",
      createdAt: now,
      updatedAt: now,
      status: { type: "idle" },
      approvalPolicy: permissionProfile?.approvalPolicy ?? normalizeApprovalPolicy(params.approvalPolicy) ?? "never",
      sandboxMode: permissionProfile?.sandboxMode ?? normalizeSandboxMode(params.sandbox) ?? "danger-full-access",
      permissionProfileId: permissionProfile?.id ?? null,
      ephemeral: isTitleOrHelper,
      threadSource: normalizeThreadSource(params.threadSource),
      agentRole: nullIfEmpty(typeof params.agentRole === "string" ? params.agentRole : null),
      agentNickname: nullIfEmpty(
        typeof params.agentNickname === "string" ? params.agentNickname : null
      ),
      baseInstructions: nullIfEmpty(
        typeof params.baseInstructions === "string" ? params.baseInstructions : null
      ),
      developerInstructions: nullIfEmpty(
        typeof params.developerInstructions === "string" ? params.developerInstructions : null
      ),
      personality: normalizePersonality(params.personality),
      // Pick the runtime backend from the chosen model — picking gpt-* in
      // the App's model dropdown flips the new thread to runtimeBackend
      // 'codex' so turns get forwarded to `codex exec`. Default 'claude'.
      runtimeBackend: selectedProviderLoop.runtimeType === "codex-proxy" || isCodexOpenAiModel(model) ? "codex" : "claude",
      codexSessionId: null
    };
    if (project) this.projects.bind(thread, project.id);
    this.store.upsertThread(thread);
    recordRunEvent("thread.started", {
      threadId: thread.id,
      cwd: thread.cwd,
      model: thread.model,
      runtimeBackend: thread.runtimeBackend,
      threadSource: thread.threadSource,
      ephemeral: thread.ephemeral
    });
    this.activePeerByThread.set(id, peer);
    this.notify(peer, { method: "thread/started", params: { thread: this.toThread(thread, []) } });
    return this.threadEnvelope(thread);
  }
  threadResume(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error("unknown thread: " + threadId);
    if (typeof params.cwd === "string" && params.cwd.length > 0) thread.cwd = params.cwd;
    const rawModel = modelFromParams(params, null);
    const model = rawModel ? normalizeSelectableModelId(rawModel, thread.model) : null;
    const reasoningEffort = reasoningEffortFromParams(params, null);
    const permissionProfileId = permissionProfileIdFromParams(params);
    const permissionProfile = permissionProfilePolicy(permissionProfileId);
    if (model) {
      const newBackend = isCodexOpenAiModel(model) ? "codex" : "claude";
      if (newBackend === thread.runtimeBackend) {
        thread.model = model;
      } else {
        debugLog("thread.resume.modelBackendMismatch", {
          threadId,
          oldModel: thread.model,
          newModel: model,
          oldBackend: thread.runtimeBackend,
          newBackend
        });
      }
    }
    if (reasoningEffort) thread.reasoningEffort = reasoningEffort;
    if (permissionProfileId) {
      thread.permissionProfileId = permissionProfileId;
      if (permissionProfile?.approvalPolicy)
        thread.approvalPolicy = permissionProfile.approvalPolicy;
      if (permissionProfile?.sandboxMode) thread.sandboxMode = permissionProfile.sandboxMode;
    } else {
      if (hasLegacyPermissionParams(params)) thread.permissionProfileId = null;
      if (typeof params.approvalPolicy === "string")
        thread.approvalPolicy = normalizeApprovalPolicy(params.approvalPolicy);
      if (typeof params.sandbox === "string")
        thread.sandboxMode = normalizeSandboxMode(params.sandbox);
    }
    if (typeof params.threadSource === "string")
      thread.threadSource = normalizeThreadSource(params.threadSource);
    if (typeof params.baseInstructions === "string")
      thread.baseInstructions = nullIfEmpty(params.baseInstructions);
    if (typeof params.developerInstructions === "string")
      thread.developerInstructions = nullIfEmpty(params.developerInstructions);
    if (typeof params.personality === "string")
      thread.personality = normalizePersonality(params.personality);
    this.store.upsertThread(thread);
    recordRunEvent("thread.resumed", {
      threadId,
      cwd: thread.cwd,
      model: thread.model,
      runtimeBackend: thread.runtimeBackend,
      threadSource: thread.threadSource,
      ephemeral: thread.ephemeral
    });
    this.activePeerByThread.set(threadId, peer);
    this.bindPeerToDescendants(peer, threadId);
    return this.threadEnvelope(
      thread,
      params.excludeTurns === true ? [] : this.store.listTurns(thread.id)
    );
  }
  threadFork(peer, params) {
    const parentId = stringOr2(params.threadId, "");
    const parent = this.store.getThread(parentId);
    if (!parent) throw new Error(`unknown thread: ${parentId}`);
    const now = nowSeconds();
    const id = newId();
    const requestedCwd = stringOr2(params.cwd, parent.cwd);
    const cwd = maybeCreateThreadWorktree(id, requestedCwd).cwd;
    const thread = {
      ...parent,
      id,
      sessionId: parent.sessionId,
      forkedFromId: parent.id,
      isPinned: false,
      sectionId: null,
      sectionEnteredAt: null,
      sectionPosition: null,
      archived: false,
      cwd,
      model: modelFromParams(params, parent.model),
      reasoningEffort: reasoningEffortFromParams(params, parent.reasoningEffort),
      claudeSessionId: parent.claudeSessionId,
      createdAt: now,
      updatedAt: now,
      status: { type: "idle" },
      approvalPolicy: permissionProfilePolicy(permissionProfileIdFromParams(params))?.approvalPolicy ?? (typeof params.approvalPolicy === "string" ? normalizeApprovalPolicy(params.approvalPolicy) : parent.approvalPolicy),
      sandboxMode: permissionProfilePolicy(permissionProfileIdFromParams(params))?.sandboxMode ?? (typeof params.sandbox === "string" ? normalizeSandboxMode(params.sandbox) : parent.sandboxMode),
      permissionProfileId: permissionProfileIdFromParams(params) ?? (hasLegacyPermissionParams(params) ? null : parent.permissionProfileId ?? null),
      ephemeral: parent.ephemeral,
      threadSource: typeof params.threadSource === "string" ? normalizeThreadSource(params.threadSource) : normalizeThreadSource(parent.threadSource),
      agentRole: nullIfEmpty(
        typeof params.agentRole === "string" ? params.agentRole : parent.agentRole
      ),
      agentNickname: nullIfEmpty(
        typeof params.agentNickname === "string" ? params.agentNickname : parent.agentNickname
      ),
      baseInstructions: nullIfEmpty(
        typeof params.baseInstructions === "string" ? params.baseInstructions : parent.baseInstructions
      ),
      developerInstructions: nullIfEmpty(
        typeof params.developerInstructions === "string" ? params.developerInstructions : parent.developerInstructions
      ),
      personality: typeof params.personality === "string" ? normalizePersonality(params.personality) : parent.personality,
      // Fork: model may flip backend (forking from claude-thread with a
      // gpt-* model = new codex-backed thread); otherwise inherit parent.
      runtimeBackend: isCodexOpenAiModel(modelFromParams(params, parent.model)) ? "codex" : "claude",
      codexSessionId: null
    };
    this.store.upsertThread(thread);
    recordRunEvent("thread.forked", {
      threadId: thread.id,
      parentThreadId: parent.id,
      cwd: thread.cwd,
      model: thread.model,
      runtimeBackend: thread.runtimeBackend,
      threadSource: thread.threadSource,
      ephemeral: thread.ephemeral
    });
    this.activePeerByThread.set(id, peer);
    this.notify(peer, { method: "thread/started", params: { thread: this.toThread(thread, []) } });
    return this.threadEnvelope(
      thread,
      params.excludeTurns === true ? [] : this.store.listTurns(parent.id)
    );
  }
  threadList(params) {
    const sourceKinds = Array.isArray(params.sourceKinds) ? params.sourceKinds.filter((value) => typeof value === "string") : [];
    const parentThreadId = typeof params.parentThreadId === "string" && params.parentThreadId.length > 0 ? params.parentThreadId : null;
    const ancestorThreadId = typeof params.ancestorThreadId === "string" && params.ancestorThreadId.length > 0 ? params.ancestorThreadId : null;
    const sortKey = params.sortKey === "updated_at" || params.sortKey === "recency_at" || params.sortKey === "section_position" ? params.sortKey : "created_at";
    const sortDirection = params.sortDirection === "asc" ? "asc" : "desc";
    const isPinned = typeof params.isPinned === "boolean" ? params.isPinned : null;
    const sectionId = params.sectionId === null ? null : typeof params.sectionId === "string" ? params.sectionId : void 0;
    const threads = this.store.listThreads({
      archived: params.archived ?? null,
      limit: numberOr(params.limit, 50),
      cursor: typeof params.cursor === "string" ? params.cursor : null,
      isPinned,
      sectionId,
      cwd: typeof params.cwd === "string" || Array.isArray(params.cwd) ? params.cwd : null,
      includeEphemeral: params.includeEphemeral === true,
      parentThreadId,
      ancestorThreadId,
      sourceKinds,
      sortKey,
      sortDirection
    });
    const last = threads.at(-1);
    const cursorValue = (thread) => sortKey === "section_position" ? thread.sectionPosition ?? thread.createdAt : sortKey === "created_at" ? thread.createdAt : thread.updatedAt;
    return {
      data: threads.filter((thread) => params.projectId === void 0 || this.projects.projectId(thread) === params.projectId).map((thread) => this.toThread(thread, [])),
      nextCursor: last && threads.length >= numberOr(params.limit, 50) ? String(cursorValue(last)) : null,
      backwardsCursor: threads[0] ? String(cursorValue(threads[0])) : null
    };
  }
  threadSectionMove(params) {
    const threadId = stringOr2(params.threadId, "");
    if (params.sectionId !== null && typeof params.sectionId !== "string")
      throw new Error("sectionId must be a string or null");
    const sectionId = params.sectionId === null ? null : params.sectionId;
    if (sectionId === "") throw new Error("sectionId must not be empty");
    const beforeThreadId = typeof params.beforeThreadId === "string" && params.beforeThreadId.length > 0 ? params.beforeThreadId : null;
    const moved = this.store.moveThreadToSection(threadId, sectionId, beforeThreadId);
    debugLog("thread.section.moved", {
      threadId,
      sectionId: moved.sectionId ?? null,
      isPinned: moved.isPinned === true,
      beforeThreadId
    });
    return {};
  }
  threadSectionList(params) {
    const requestedLimit = numberOr(params.limit, 100);
    const limit = Number.isFinite(requestedLimit) ? Math.max(1, Math.min(Math.floor(requestedLimit), 200)) : 100;
    const offset = typeof params.cursor === "string" ? Number(params.cursor) : 0;
    if (!Number.isSafeInteger(offset) || offset < 0) throw new Error("invalid section cursor");
    const sections = this.store.listSections(limit, String(offset));
    const nextOffset = offset + sections.length;
    const hasMore = this.store.listSections(1, String(nextOffset)).length > 0;
    return { data: sections, nextCursor: hasMore ? String(nextOffset) : null };
  }
  threadSectionCreate(params) {
    const name2 = stringOr2(params.name, "");
    const section = this.store.createSection(
      newId(),
      name2,
      this.sectionAppearance(params.appearance)
    );
    return { section };
  }
  threadSectionUpdate(params) {
    const sectionId = stringOr2(params.sectionId, "");
    const existing = this.store.getSection(sectionId);
    if (!existing) throw new Error(`unknown section: ${sectionId}`);
    const name2 = typeof params.name === "string" ? params.name : existing.name;
    const appearance = params.appearance === void 0 ? existing.appearance : this.sectionAppearance(params.appearance);
    return { section: this.store.updateSection(sectionId, name2, appearance) };
  }
  threadSectionDelete(params) {
    this.store.deleteSection(stringOr2(params.sectionId, ""));
    return {};
  }
  sectionAppearance(value) {
    if (value == null) return null;
    const record = asRecord3(value);
    return {
      color: typeof record.color === "string" ? record.color : null,
      icon: typeof record.icon === "string" ? record.icon : null
    };
  }
  threadRead(params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error("unknown thread: " + threadId);
    const includeTurns = params.includeTurns !== false || thread.threadSource === "subagent";
    const turns = includeTurns ? this.store.listTurns(threadId) : [];
    return { thread: this.toThread(thread, turns) };
  }
  threadTurnsList(params) {
    const threadId = stringOr2(params.threadId, "");
    const itemsView = turnItemsView(params.itemsView);
    return {
      data: this.store.listTurns(threadId).map((turn) => this.toTurnView(turn, itemsView)),
      nextCursor: null,
      backwardsCursor: null
    };
  }
  // Ivy's shared transcript reader uses the newer paged item method. Read the
  // adapter's persisted turn items; never synthesize a successful empty page.
  threadItemsList(params) {
    const threadId = stringOr2(params.threadId, "");
    if (!this.store.getThread(threadId)) throw new Error("unknown thread: " + threadId);
    const requestedTurnId = typeof params.turnId === "string" ? params.turnId : null;
    const turns = this.store.listTurns(threadId);
    if (requestedTurnId && !turns.some((turn) => turn.id === requestedTurnId))
      throw new Error("turn does not belong to thread: " + requestedTurnId);
    const entries = turns.filter((turn) => !requestedTurnId || turn.id === requestedTurnId).flatMap((turn) => turn.items.map((item) => ({ turnId: turn.id, item })));
    if (params.sortDirection === "desc") entries.reverse();
    const offset = params.cursor == null ? 0 : Number(params.cursor);
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > entries.length)
      throw new Error("invalid item cursor");
    const limit = params.limit == null ? 100 : Number(params.limit);
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100)
      throw new Error("invalid item limit");
    const data = entries.slice(offset, offset + limit);
    return {
      data,
      nextCursor: offset + data.length < entries.length ? String(offset + data.length) : null,
      backwardsCursor: data.length ? String(offset) : null
    };
  }
  threadTurnItemsList(params) {
    const turnId = stringOr2(params.turnId, "");
    const turn = this.store.getTurn(turnId);
    return {
      data: turn?.items ?? [],
      nextCursor: null,
      backwardsCursor: null
    };
  }
  threadNameSet(params) {
    const threadId = stringOr2(params.threadId, "");
    const name2 = params.name == null ? null : String(params.name);
    this.store.updateThreadName(threadId, name2);
    this.notifyThread(threadId, {
      method: "thread/name/updated",
      params: { threadId, threadName: name2 ?? void 0 }
    });
    return {};
  }
  threadArchive(params, archived) {
    const threadId = stringOr2(params.threadId, "");
    this.store.setArchived(threadId, archived);
    this.notifyThread(threadId, {
      method: archived ? "thread/archived" : "thread/unarchived",
      params: { threadId }
    });
    if (!archived) {
      const thread = this.store.getThread(threadId);
      if (!thread) throw new Error(`unknown thread: ${threadId}`);
      return { thread: this.toThread(thread, this.store.listTurns(threadId)) };
    }
    this.clearThreadState(threadId);
    return {};
  }
  // Drops per-thread in-memory state (session-scoped command approvals, token
  // usage tallies, goals, elicitation counts) so an archived thread does not
  // leak entries for the lifetime of the process.
  clearThreadState(threadId) {
    this.commandSessionAllow.delete(threadId);
    this.tokenUsageByThread.delete(threadId);
    this.goals.delete(threadId);
    this.elicitationCounts.delete(threadId);
  }
  threadUnsubscribe(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const active = this.activePeerByThread.get(threadId);
    if (!active) return { status: "notSubscribed" };
    if (active.id !== peer.id) return { status: "notSubscribed" };
    this.activePeerByThread.delete(threadId);
    this.notify(peer, { method: "thread/closed", params: { threadId } });
    return { status: "unsubscribed" };
  }
  threadLoadedList(params) {
    const loaded = Array.from(this.activePeerByThread.keys());
    const limit = numberOr(params.limit, loaded.length || 50);
    return {
      data: loaded.slice(0, limit),
      nextCursor: loaded.length > limit ? String(limit) : null
    };
  }
  // Codex App calls thread/inject_items to push hidden context into a thread's
  // model history — typically file-attachment ingestion, "pin this output as
  // future context", or App-side memory consolidation. Items are raw Responses
  // API entries (free-form JSON). Without an implementation the App's
  // ingestion just disappears, breaking any feature that relies on it.
  //
  // Approach: synthesize an injected turn carrying a single agentMessage that
  // recaps the items as a human-readable block. That turn becomes part of the
  // thread's transcript so the next runRuntimeTurn picks it up as prior
  // conversation context, AND it's visible in thread/read so the user can
  // confirm what was added. We pick agentMessage (instead of a custom type)
  // for App-compatibility — every Codex App build renders it without needing
  // a new ThreadItem variant.
  threadInjectItems(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error(`unknown thread: ${threadId}`);
    const items = Array.isArray(params.items) ? params.items : [];
    if (items.length === 0) return {};
    const now = nowSeconds();
    const turnId = newId();
    const itemId = newId();
    const summary = items.map((raw) => summarizeInjectedItem(raw)).filter(Boolean).join("\n\n");
    const text = summary.length > 0 ? summary : `[adapter] ${items.length} item(s) injected via thread/inject_items`;
    const agentItem = {
      type: "agentMessage",
      id: itemId,
      text,
      phase: null,
      memoryCitation: null
    };
    const turn = {
      id: turnId,
      threadId,
      status: "completed",
      startedAt: now,
      completedAt: now,
      durationMs: 0,
      items: [agentItem],
      diff: "",
      error: null
    };
    this.store.upsertTurn(turn);
    thread.updatedAt = now;
    this.store.upsertThread(thread);
    queueMicrotask(() => {
      this.notify(peer, {
        method: "turn/started",
        params: { threadId, turn: this.toLifecycleTurn(turn) }
      });
      this.notify(peer, {
        method: "item/completed",
        params: { threadId, turnId, item: agentItem, completedAtMs: nowMillis() }
      });
      this.notify(peer, {
        method: "turn/completed",
        params: { threadId, turn: this.toLifecycleTurn(turn) }
      });
    });
    debugLog("thread.inject_items", { threadId, count: items.length });
    return {};
  }
  threadGoalSet(params) {
    const threadId = stringOr2(params.threadId, "");
    const existing = this.goals.get(threadId);
    const now = nowSeconds();
    const goal = {
      threadId,
      objective: stringOr2(params.objective, String(existing?.objective ?? "")),
      status: typeof params.status === "string" ? params.status : existing?.status ?? "active",
      tokenBudget: typeof params.tokenBudget === "number" ? params.tokenBudget : existing?.tokenBudget ?? null,
      tokensUsed: existing?.tokensUsed ?? 0,
      timeUsedSeconds: existing?.timeUsedSeconds ?? 0,
      createdAt: existing?.createdAt ?? now,
      updatedAt: now
    };
    this.goals.set(threadId, goal);
    this.notifyThread(threadId, { method: "thread/goal/updated", params: { threadId, goal } });
    return { goal };
  }
  threadGoalGet(params) {
    return { goal: this.goals.get(stringOr2(params.threadId, "")) ?? null };
  }
  threadGoalClear(params) {
    const threadId = stringOr2(params.threadId, "");
    const cleared = this.goals.delete(threadId);
    if (cleared)
      this.notifyThread(threadId, { method: "thread/goal/cleared", params: { threadId } });
    return { cleared };
  }
  threadAdjustElicitation(params, delta) {
    const threadId = stringOr2(params.threadId, "");
    const next = Math.max(0, (this.elicitationCounts.get(threadId) ?? 0) + delta);
    this.elicitationCounts.set(threadId, next);
    return { count: next, paused: next > 0 };
  }
  threadMetadataUpdate(params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error(`unknown thread: ${threadId}`);
    const rawModel = modelFromParams(params, null);
    const model = rawModel ? normalizeSelectableModelId(rawModel, thread.model) : null;
    const reasoningEffort = reasoningEffortFromParams(params, null);
    if (model) {
      const newBackend = isCodexOpenAiModel(model) ? "codex" : "claude";
      thread.runtimeBackend = newBackend;
      thread.model = model;
    }
    if (reasoningEffort) thread.reasoningEffort = reasoningEffort;
    if (typeof params.approvalPolicy === "string")
      thread.approvalPolicy = normalizeApprovalPolicy(params.approvalPolicy);
    if (typeof params.sandbox === "string")
      thread.sandboxMode = normalizeSandboxMode(params.sandbox);
    if (typeof params.isPinned === "boolean") {
      const currentlyPinned = thread.sectionId === PINNED_SECTION_ID || thread.isPinned === true;
      if (currentlyPinned !== params.isPinned) {
        const moved = this.store.moveThreadToSection(
          threadId,
          params.isPinned ? PINNED_SECTION_ID : null,
          null
        );
        thread.isPinned = moved.isPinned;
        thread.sectionId = moved.sectionId;
        thread.sectionEnteredAt = moved.sectionEnteredAt;
        thread.sectionPosition = moved.sectionPosition;
      }
    }
    if (typeof params.baseInstructions === "string")
      thread.baseInstructions = nullIfEmpty(params.baseInstructions);
    if (typeof params.developerInstructions === "string")
      thread.developerInstructions = nullIfEmpty(params.developerInstructions);
    if (typeof params.personality === "string")
      thread.personality = normalizePersonality(params.personality);
    thread.updatedAt = nowSeconds();
    this.store.upsertThread(thread);
    return this.threadEnvelope(thread, this.store.listTurns(threadId));
  }
  threadSettingsUpdate(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error(`unknown thread: ${threadId}`);
    const rawModel = modelFromParams(params, null);
    const model = rawModel ? normalizeSelectableModelId(rawModel, thread.model) : null;
    if (model) {
      thread.runtimeBackend = isCodexOpenAiModel(model) ? "codex" : "claude";
      thread.model = model;
    }
    const reasoningEffort = reasoningEffortFromParams(params, null);
    if (reasoningEffort) thread.reasoningEffort = reasoningEffort;
    if (typeof params.personality === "string")
      thread.personality = normalizePersonality(params.personality);
    const collaborationMode = asRecord3(params.collaborationMode);
    if (collaborationMode.mode === "default" || collaborationMode.mode === "plan")
      thread.collaborationMode = collaborationMode.mode;
    const collaborationSettings = asRecord3(collaborationMode.settings);
    const developerInstructions = typeof collaborationSettings.developer_instructions === "string" ? collaborationSettings.developer_instructions : typeof collaborationSettings.developerInstructions === "string" ? collaborationSettings.developerInstructions : null;
    if (developerInstructions !== null)
      thread.developerInstructions = nullIfEmpty(developerInstructions);
    const permissionProfileId = permissionProfileIdFromParams(params);
    const permissionProfile = permissionProfilePolicy(permissionProfileId);
    if (permissionProfileId) thread.permissionProfileId = permissionProfileId;
    else if (hasLegacyPermissionParams(params)) thread.permissionProfileId = null;
    if (permissionProfile?.approvalPolicy) thread.approvalPolicy = permissionProfile.approvalPolicy;
    if (permissionProfile?.sandboxMode) thread.sandboxMode = permissionProfile.sandboxMode;
    if (typeof params.approvalPolicy === "string" && !permissionProfile)
      thread.approvalPolicy = normalizeApprovalPolicy(params.approvalPolicy);
    const sandboxMode = sandboxFromTurnParams(params);
    if (sandboxMode && !permissionProfile) thread.sandboxMode = sandboxMode;
    if (typeof params.cwd === "string" && params.cwd.length > 0) thread.cwd = params.cwd;
    this.store.upsertThread(thread);
    const activePermissionProfileId = threadPermissionProfileId(
      thread.permissionProfileId,
      thread.approvalPolicy,
      thread.sandboxMode
    );
    this.notify(peer, {
      method: "thread/settings/updated",
      params: {
        threadId,
        threadSettings: {
          cwd: thread.cwd,
          approvalPolicy: thread.approvalPolicy ?? "on-request",
          approvalsReviewer: "user",
          sandboxPolicy: sandboxEnvelope(thread.sandboxMode, thread.cwd),
          activePermissionProfile: activePermissionProfileId ? { id: activePermissionProfileId, extends: null } : null,
          model: thread.model,
          modelProvider: thread.modelProvider,
          serviceTier: null,
          effort: thread.reasoningEffort,
          summary: null,
          collaborationMode: {
            mode: thread.collaborationMode ?? "default",
            settings: {
              model: thread.model,
              reasoning_effort: thread.reasoningEffort,
              developer_instructions: thread.developerInstructions
            }
          },
          personality: thread.personality
        }
      }
    });
    return {};
  }
  threadRollback(params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error(`unknown thread: ${threadId}`);
    const numTurns = typeof params.numTurns === "number" && params.numTurns >= 1 ? Math.floor(params.numTurns) : 0;
    if (numTurns > 0) {
      const dropped = this.store.deleteRecentTurns(threadId, numTurns);
      debugLog("thread.rollback", { threadId, requested: numTurns, dropped });
    }
    return { thread: this.toThread(thread, this.store.listTurns(threadId)) };
  }
  threadShellCommand(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    const command = stringOr2(params.command, "");
    if (!command) return {};
    const shell = process.env.SHELL || "/bin/sh";
    const cwd = thread?.cwd ?? process.cwd();
    const processId = newId();
    debugLog("thread.shellCommand.start", { threadId, processId, cwd, command });
    const child = spawn3(shell, ["-lc", command], { cwd, env: process.env, stdio: "pipe" });
    this.commandProcesses.set(processId, child);
    child.stdout?.on(
      "data",
      (chunk) => this.notify(peer, {
        method: "command/exec/outputDelta",
        params: {
          processId,
          stream: "stdout",
          deltaBase64: Buffer.from(chunk).toString("base64"),
          capReached: false
        }
      })
    );
    child.stderr?.on(
      "data",
      (chunk) => this.notify(peer, {
        method: "command/exec/outputDelta",
        params: {
          processId,
          stream: "stderr",
          deltaBase64: Buffer.from(chunk).toString("base64"),
          capReached: false
        }
      })
    );
    child.once(
      "error",
      (error) => debugLog("thread.shellCommand.error", { threadId, processId, error: error.message })
    );
    child.once("close", (code, signal) => {
      debugLog("thread.shellCommand.close", { threadId, processId, code, signal });
      this.commandProcesses.delete(processId);
    });
    return {};
  }
  threadBackgroundTerminalsClean(params) {
    debugLog("thread.backgroundTerminals.clean", {
      threadId: stringOr2(params.threadId, ""),
      activeCommandProcesses: this.commandProcesses.size,
      activeProcessHandles: this.processHandles.size
    });
    return {};
  }
  reviewStart(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error(`unknown thread: ${threadId}`);
    this.activePeerByThread.set(threadId, peer);
    const turnId = newId();
    const review = reviewLabel(params.target);
    const prompt = reviewPrompt(params.target);
    const userItem = {
      type: "userMessage",
      id: turnId,
      content: [{ type: "text", text: review, text_elements: [] }]
    };
    const entered = { type: "enteredReviewMode", id: newId(), review };
    const turn = {
      id: turnId,
      threadId,
      status: "inProgress",
      startedAt: nowSeconds(),
      completedAt: null,
      durationMs: null,
      items: [userItem, entered],
      diff: "",
      error: null
    };
    this.store.upsertTurn(turn);
    recordRunEvent("turn.started", {
      threadId,
      turnId,
      cwd: thread.cwd,
      model: thread.model,
      runtimeBackend: thread.runtimeBackend,
      purpose: params.outputSchema == null ? "normal" : "summary"
    });
    this.activeTurnByThread.set(threadId, turnId);
    this.setThreadStatus(peer, threadId, { type: "active", activeFlags: [] });
    const responseTurn = this.toLifecycleTurn(turn, [userItem]);
    setImmediate(() => {
      this.notify(peer, {
        method: "turn/started",
        params: { threadId, turn: this.toLifecycleTurn(turn) }
      });
      this.notify(peer, {
        method: "item/started",
        params: { threadId, turnId, item: entered, startedAtMs: nowMillis() }
      });
      void this.runRuntimeTurn(peer, thread, turn, prompt, {
        model: thread.model,
        effort: thread.reasoningEffort
      }).catch((error) => {
        const completed = this.store.completeTurn(turnId, "failed", { message: error.message }) ?? turn;
        recordRunEvent("turn.failed", {
          threadId,
          turnId,
          runtimeBackend: thread.runtimeBackend,
          error: { message: error.message }
        });
        this.notify(peer, {
          method: "error",
          params: { threadId, turnId, willRetry: false, error: { message: error.message } }
        });
        this.notify(peer, {
          method: "turn/completed",
          params: { threadId, turn: this.toLifecycleTurn(completed) }
        });
        this.clearActiveTurn(threadId);
        this.setThreadStatus(peer, threadId, { type: "idle" });
      });
    });
    return { turn: responseTurn, reviewThreadId: threadId };
  }
  threadCompactStart(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error(`unknown thread: ${threadId}`);
    const turnId = newId();
    const compactItem = { type: "contextCompaction", id: newId() };
    const turn = {
      id: turnId,
      threadId,
      status: "inProgress",
      startedAt: nowSeconds(),
      completedAt: null,
      durationMs: null,
      items: [compactItem],
      diff: "",
      error: null
    };
    this.store.upsertTurn(turn);
    recordRunEvent("turn.started", {
      threadId,
      turnId,
      cwd: thread.cwd,
      model: thread.model,
      runtimeBackend: thread.runtimeBackend,
      purpose: params.outputSchema == null ? "normal" : "summary"
    });
    this.activeTurnByThread.set(threadId, turnId);
    this.setThreadStatus(peer, threadId, { type: "active", activeFlags: [] });
    setImmediate(() => {
      this.notify(peer, {
        method: "turn/started",
        params: { threadId, turn: this.toLifecycleTurn(turn) }
      });
      this.notify(peer, {
        method: "item/started",
        params: { threadId, turnId, item: compactItem, startedAtMs: nowMillis() }
      });
      const agentItem = {
        type: "agentMessage",
        id: newId(),
        text: "",
        phase: null,
        memoryCitation: null
      };
      this.store.appendItem(turnId, agentItem);
      this.notify(peer, {
        method: "item/started",
        params: { threadId, turnId, item: agentItem, startedAtMs: nowMillis() }
      });
      void this.runCompactTurn(peer, thread, turnId, agentItem.id, compactItem).catch((error) => {
        debugLog("thread.compact.fallback", { threadId, error: error?.message ?? String(error) });
        const fallback = compactSummary(thread, this.store.listTurns(threadId));
        this.store.updateItem(
          turnId,
          agentItem.id,
          (item) => item.type === "agentMessage" ? { ...item, text: fallback } : item
        );
        this.notify(peer, {
          method: "item/agentMessage/delta",
          params: { threadId, turnId, itemId: agentItem.id, delta: fallback }
        });
      }).finally(() => {
        this.notify(peer, {
          method: "item/completed",
          params: { threadId, turnId, item: compactItem, completedAtMs: nowMillis() }
        });
        const finalAgent = this.store.getTurn(turnId)?.items.find((i) => i.id === agentItem.id) ?? agentItem;
        this.notify(peer, {
          method: "item/completed",
          params: { threadId, turnId, item: finalAgent, completedAtMs: nowMillis() }
        });
        const completed = this.store.completeTurn(turnId, "completed") ?? turn;
        this.clearActiveTurn(threadId);
        this.setThreadStatus(peer, threadId, { type: "idle" });
        this.notify(peer, {
          method: "turn/completed",
          params: { threadId, turn: this.toLifecycleTurn(completed) }
        });
        this.notify(peer, { method: "thread/compacted", params: { threadId } });
      });
    });
    return {};
  }
  // Calls into the runtime with a structured "give me a 1-paragraph summary"
  // prompt against the summary model alias (haiku). Streams the text into the
  // placeholder agent message via item/agentMessage/delta. Errors propagate
  // so the threadCompactStart fallback can substitute the local snippet.
  async runCompactTurn(peer, thread, turnId, agentItemId, compactItem) {
    const turns = this.store.listTurns(thread.id);
    const promptBody = compactSummary(thread, turns);
    const compactPrompt = [
      "You are summarizing a Codex / Claude Code conversation so the user can keep context after compaction.",
      "Produce ONE concise paragraph (\u2264 6 sentences) covering goals, decisions, files touched, and outstanding work.",
      "Skip greetings; do not invent details that are not in the snippets below.",
      "",
      promptBody
    ].join("\n");
    let collected = "";
    await this.runtime.runTurn(
      {
        threadId: thread.id,
        turnId,
        purpose: "compact",
        prompt: compactPrompt,
        cwd: thread.cwd,
        runtimeType: null,
        model: resolveClaudeModel(thread.model, "summary"),
        effort: resolveClaudeEffort(thread.reasoningEffort ?? null),
        claudeSessionId: null,
        forkSession: false,
        mcpServers: null,
        allowedTools: ["Read", "Glob", "Grep"],
        addDirs: [],
        enableFileCheckpointing: false,
        outputFormat: null,
        approvalPolicy: "never",
        sandboxMode: "read-only",
        systemPromptAddendum: null,
        planMode: false,
        imageInputs: []
      },
      {
        onEvent: async (event) => {
          if (event.type === "text_delta" && event.delta) {
            collected += event.delta;
            this.store.updateItem(
              turnId,
              agentItemId,
              (item) => item.type === "agentMessage" ? { ...item, text: item.text + event.delta } : item
            );
            this.notify(peer, {
              method: "item/agentMessage/delta",
              params: { threadId: thread.id, turnId, itemId: agentItemId, delta: event.delta }
            });
          }
          if (event.type === "error") throw new Error(event.message);
          if (event.type === "completed" && !event.success) {
            throw new Error(event.result ?? "compaction turn failed");
          }
        },
        // Compaction never asks for approvals — it's read-only summarisation.
        onPermissionRequest: async () => ({ decision: "accept" }),
        // Compaction shouldn't ever invoke AskUserQuestion; if it does, return
        // an empty answer so the model proceeds with its summary.
        onUserInputRequest: async (event) => ({
          answers: Object.fromEntries(event.questions.map((q) => [q.id, { answers: [] }]))
        })
      }
    );
    if (!collected.trim()) {
      void compactItem;
      throw new Error("compaction returned no content");
    }
  }
  async turnStart(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const thread = this.store.getThread(threadId);
    if (!thread) throw new Error(`unknown thread: ${threadId}`);
    this.activePeerByThread.set(threadId, peer);
    if (process.env.CLAUDE_CODEX_DISABLE_CODEX_PROXY === "1" && (thread.runtimeBackend === "codex" || isCodexOpenAiModel(modelFromParams(params, thread.model))))
      throw new Error("Codex proxy models are disabled for this Claude adapter");
    const turnId = newId();
    const input = Array.isArray(params.input) ? params.input : [];
    const { textPrompt, images } = extractImageInputs(input);
    const prompt = textPrompt || textFromInput(input);
    const workflowCommand = parseWorkflowCommand(prompt);
    const requestedMode = asRecord3(params.collaborationMode).mode;
    if (requestedMode === "default" || requestedMode === "plan") thread.collaborationMode = requestedMode;
    if (typeof params.cwd === "string" && params.cwd.length > 0) thread.cwd = params.cwd;
    const model = modelFromParams(params, thread.model);
    const reasoningEffort = reasoningEffortFromParams(params, thread.reasoningEffort);
    const permissionProfileId = permissionProfileIdFromParams(params);
    const permissionProfile = permissionProfilePolicy(permissionProfileId);
    if (model) thread.model = model;
    if (reasoningEffort) thread.reasoningEffort = reasoningEffort;
    if (permissionProfileId) {
      thread.permissionProfileId = permissionProfileId;
      if (permissionProfile?.approvalPolicy)
        thread.approvalPolicy = permissionProfile.approvalPolicy;
      if (permissionProfile?.sandboxMode) thread.sandboxMode = permissionProfile.sandboxMode;
    } else {
      if (hasLegacyPermissionParams(params)) thread.permissionProfileId = null;
      if (typeof params.approvalPolicy === "string")
        thread.approvalPolicy = normalizeApprovalPolicy(params.approvalPolicy);
      const requestedSandbox = sandboxFromTurnParams(params);
      if (requestedSandbox) thread.sandboxMode = requestedSandbox;
    }
    this.store.upsertThread(thread);
    if (!thread.preview && prompt) {
      thread.preview = prompt.slice(0, 200);
      thread.updatedAt = nowSeconds();
      this.store.upsertThread(thread);
    }
    const initialItems = [{ type: "userMessage", id: newId(), content: input }];
    const imageItems = [];
    for (const img of images) {
      if (img.kind === "base64" && img.displayPath.startsWith("/")) {
        imageItems.push({ type: "imageView", id: newId(), path: img.displayPath });
      }
    }
    initialItems.push(...imageItems);
    const turn = {
      id: turnId,
      threadId,
      status: "inProgress",
      startedAt: nowSeconds(),
      completedAt: null,
      durationMs: null,
      items: initialItems,
      diff: "",
      error: null
    };
    this.store.upsertTurn(turn);
    recordRunEvent("turn.started", {
      threadId,
      turnId,
      cwd: thread.cwd,
      model: thread.model,
      runtimeBackend: thread.runtimeBackend,
      purpose: params.outputSchema == null ? "normal" : "summary"
    });
    this.activeTurnByThread.set(threadId, turnId);
    this.setThreadStatus(peer, threadId, { type: "active", activeFlags: [] });
    const publicTurn = this.toLifecycleTurn(turn);
    setImmediate(() => {
      this.notify(peer, { method: "turn/started", params: { threadId, turn: publicTurn } });
      for (const item of imageItems) {
        this.notify(peer, {
          method: "item/started",
          params: { threadId, turnId, item, startedAtMs: nowMillis() }
        });
        this.notify(peer, {
          method: "item/completed",
          params: { threadId, turnId, item, completedAtMs: nowMillis() }
        });
      }
      if (params.outputSchema == null && workflowCommand?.type === "list") {
        this.completeWorkflowListTurn(peer, thread, turn);
        return;
      }
      void this.runRuntimeTurn(peer, thread, turn, prompt, {
        ...params,
        _imageInputs: images
      }).catch((error) => {
        const current = this.store.getTurn(turnId);
        if (current && current.status !== "inProgress") return;
        const completed = this.store.completeTurn(turnId, "failed", { message: error.message }) ?? turn;
        this.notify(peer, {
          method: "error",
          params: { threadId, turnId, willRetry: false, error: { message: error.message } }
        });
        this.notify(peer, {
          method: "turn/completed",
          params: { threadId, turn: this.toLifecycleTurn(completed) }
        });
        this.clearActiveTurn(threadId);
        this.setThreadStatus(peer, threadId, { type: "idle" });
      });
    });
    return { turn: publicTurn };
  }
  completeWorkflowListTurn(peer, thread, turn) {
    const itemId = newId();
    const emptyItem = {
      type: "agentMessage",
      id: itemId,
      text: "",
      phase: null,
      memoryCitation: null
    };
    this.store.appendItem(turn.id, emptyItem);
    this.notify(peer, {
      method: "item/started",
      params: { threadId: thread.id, turnId: turn.id, item: emptyItem, startedAtMs: nowMillis() }
    });
    const text = this.workflowListText(thread.id, turn.id);
    const completedItem = { ...emptyItem, text };
    this.store.updateItem(turn.id, itemId, () => completedItem);
    this.notify(peer, {
      method: "item/agentMessage/delta",
      params: { threadId: thread.id, turnId: turn.id, itemId, delta: text }
    });
    this.notify(peer, {
      method: "item/completed",
      params: {
        threadId: thread.id,
        turnId: turn.id,
        item: completedItem,
        completedAtMs: nowMillis()
      }
    });
    const completed = this.store.completeTurn(turn.id, "completed") ?? turn;
    recordRunEvent("turn.completed", {
      threadId: thread.id,
      turnId: turn.id,
      runtimeBackend: thread.runtimeBackend,
      durationMs: completed.durationMs,
      command: "workflows.list"
    });
    this.clearActiveTurn(thread.id);
    this.setThreadStatus(peer, thread.id, { type: "idle" });
    this.notify(peer, {
      method: "turn/completed",
      params: { threadId: thread.id, turn: this.toLifecycleTurn(completed) }
    });
  }
  workflowListText(threadId, currentTurnId) {
    const runs = this.store.listTurns(threadId).filter((turn) => turn.id !== currentTurnId).map((turn) => {
      const userItem = turn.items.find((item) => item.type === "userMessage");
      const prompt = userItem?.type === "userMessage" ? textFromInput(userItem.content) : "";
      const command = parseWorkflowCommand(prompt);
      if (command?.type !== "run") return null;
      const childIds = /* @__PURE__ */ new Set();
      for (const item of turn.items) {
        if (item.type !== "collabAgentToolCall" || item.tool !== "spawnAgent") continue;
        for (const childId of item.receiverThreadIds) childIds.add(childId);
      }
      const agents = Array.from(childIds).map((childId) => {
        const child = this.store.getThread(childId);
        const latestTurn = this.store.listTurns(childId).at(-1);
        return {
          nickname: child?.agentNickname ?? childId.slice(0, 12),
          role: child?.agentRole ?? "subagent",
          status: latestTurn?.status ?? child?.status.type ?? "unknown"
        };
      });
      return { turn, prompt: command.prompt, agents };
    }).filter((run) => run !== null).reverse().slice(0, 20);
    if (runs.length === 0) {
      return [
        "No workflow runs in this task.",
        "",
        "Start one with `/workflows <task>`. The adapter runs it through Claude ultracode and exposes its agents as reviewable Codex subagent tasks."
      ].join("\n");
    }
    const lines = ["Workflow runs in this task:"];
    for (const run of runs) {
      const task = run.prompt.replace(/\s+/g, " ").slice(0, 120);
      lines.push(
        `- ${run.turn.id.slice(0, 12)} \xB7 ${run.turn.status} \xB7 ${run.agents.length} agent(s) \xB7 ${task}`
      );
      if (run.agents.length > 0) {
        lines.push(
          `  Agents: ${run.agents.map((agent) => `${agent.nickname} (${agent.role}, ${agent.status})`).join(", ")}`
        );
      }
    }
    lines.push("", "Open the Agent items in the workflow turn to review each child task.");
    return lines.join("\n");
  }
  async runRuntimeTurn(peer, thread, turn, prompt, params) {
    const itemIds = /* @__PURE__ */ new Map();
    let agentItemId = null;
    let reasoningItemId = null;
    let hasTextOutput = false;
    const noticesSeen = /* @__PURE__ */ new Set();
    const commandOutputSeen = /* @__PURE__ */ new Set();
    const subagentContexts = /* @__PURE__ */ new Map();
    const activeSubagents = /* @__PURE__ */ new Set();
    let workflowInFlight = false;
    const workflowLaunchToolUseIds = /* @__PURE__ */ new Set();
    const itemStartedAtMs = /* @__PURE__ */ new Map();
    const collectedMetrics = {
      apiDurationMs: null,
      numTurns: null,
      costUsd: null,
      set: false
    };
    const forkSession = thread.forkedFromId != null && thread.claudeSessionId != null && this.store.listTurns(thread.id).length <= 1;
    const planMode = thread.collaborationMode === "plan" || params.planMode === true;
    let planItemId = null;
    const ensurePlanItem = () => {
      if (planItemId) return planItemId;
      planItemId = newId();
      const item = { type: "plan", id: planItemId, text: "" };
      this.store.appendItem(turn.id, item);
      this.notify(peer, {
        method: "item/started",
        params: { threadId: thread.id, turnId: turn.id, item, startedAtMs: nowMillis() }
      });
      return planItemId;
    };
    const ensureAgentItem = () => {
      if (agentItemId) return agentItemId;
      agentItemId = newId();
      const item = {
        type: "agentMessage",
        id: agentItemId,
        text: "",
        phase: null,
        memoryCitation: null
      };
      this.store.appendItem(turn.id, item);
      this.notify(peer, {
        method: "item/started",
        params: { threadId: thread.id, turnId: turn.id, item, startedAtMs: nowMillis() }
      });
      return agentItemId;
    };
    const ensureReasoningItem = () => {
      if (reasoningItemId) return reasoningItemId;
      reasoningItemId = newId();
      const item = {
        type: "reasoning",
        id: reasoningItemId,
        summary: [],
        content: [""]
      };
      this.store.appendItem(turn.id, item);
      this.notify(peer, {
        method: "item/started",
        params: { threadId: thread.id, turnId: turn.id, item, startedAtMs: nowMillis() }
      });
      return reasoningItemId;
    };
    const completeReasoningItem = () => {
      if (!reasoningItemId) return;
      const item = this.store.getTurn(turn.id)?.items.find((item2) => item2.id === reasoningItemId);
      if (item)
        this.notify(peer, {
          method: "item/completed",
          params: { threadId: thread.id, turnId: turn.id, item, completedAtMs: nowMillis() }
        });
      reasoningItemId = null;
    };
    const completeAgentItem = (phase) => {
      if (!agentItemId) return;
      const updated = this.store.updateItem(
        turn.id,
        agentItemId,
        (item2) => item2.type === "agentMessage" ? { ...item2, phase } : item2
      );
      const item = updated?.items.find((item2) => item2.id === agentItemId);
      if (item)
        this.notify(peer, {
          method: "item/completed",
          params: { threadId: thread.id, turnId: turn.id, item, completedAtMs: nowMillis() }
        });
      agentItemId = null;
    };
    const completeMessage = (phase = "commentary") => {
      completeReasoningItem();
      completeAgentItem(phase);
    };
    const permissionProfile = permissionProfilePolicy(permissionProfileIdFromParams(params));
    const approvalPolicy = permissionProfile?.approvalPolicy ?? (typeof params.approvalPolicy === "string" ? normalizeApprovalPolicy(params.approvalPolicy) : null) ?? thread.approvalPolicy;
    const sandboxMode = permissionProfile?.sandboxMode ?? sandboxFromTurnParams(params) ?? thread.sandboxMode;
    const baseInstructions = (typeof params.baseInstructions === "string" ? params.baseInstructions : null) ?? thread.baseInstructions;
    const developerInstructions = (typeof params.developerInstructions === "string" ? params.developerInstructions : null) ?? thread.developerInstructions;
    const personality = (typeof params.personality === "string" ? normalizePersonality(params.personality) : null) ?? thread.personality;
    const turnPurpose = params.outputSchema == null ? "normal" : "summary";
    const desktopPresentation = turnPurpose === "normal" && thread.runtimeBackend !== "codex";
    let systemPromptAddendum = null;
    let effectivePrompt = prompt;
    if (thread.forkedFromId != null && developerInstructions) {
      const parentThread = this.store.getThread(thread.forkedFromId);
      const parentDev = parentThread?.developerInstructions ?? null;
      if (developerInstructions !== parentDev) {
        systemPromptAddendum = buildSystemPromptAddendum({
          baseInstructions: parentThread?.baseInstructions ?? baseInstructions,
          developerInstructions: parentDev,
          personality: parentThread?.personality ?? personality,
          desktopPresentation
        });
        effectivePrompt = `<side-conversation-instructions>
${developerInstructions}
</side-conversation-instructions>

${prompt}`;
      } else {
        systemPromptAddendum = buildSystemPromptAddendum({
          baseInstructions,
          developerInstructions,
          personality,
          desktopPresentation
        });
      }
    } else {
      systemPromptAddendum = buildSystemPromptAddendum({
        baseInstructions,
        developerInstructions,
        personality,
        desktopPresentation
      });
    }
    const rawTurnModel = stringOr2(params.model, thread.model);
    const isCodexThread = thread.runtimeBackend === "codex" && process.env.CLAUDE_CODEX_MOCK !== "1";
    const resolvedModel = isCodexThread ? rawTurnModel : resolveClaudeModel(rawTurnModel, params.outputSchema == null ? "normal" : "summary");
    const resolvedEffort = resolveClaudeEffort(
      typeof params.effort === "string" ? params.effort : thread.reasoningEffort ?? process.env.CLAUDE_CODEX_EFFORT ?? null
    );
    debugLog("turn.runtime.applied", {
      threadId: thread.id,
      turnId: turn.id,
      paramsModel: params.model ?? null,
      paramsEffort: params.effort ?? null,
      threadModel: thread.model,
      threadEffort: thread.reasoningEffort,
      envDefaultModel: process.env.CLAUDE_CODEX_DEFAULT_MODEL ?? null,
      envDefaultEffort: process.env.CLAUDE_CODEX_DEFAULT_EFFORT ?? process.env.CLAUDE_CODEX_EFFORT ?? null,
      resolvedModel,
      resolvedEffort
    });
    this.subagentStateByTurn.set(turn.id, {
      thread,
      turn,
      contexts: subagentContexts,
      active: activeSubagents
    });
    let acceptRuntimeEvents = true;
    let watchdogTimer = null;
    let resolveWatchdog = null;
    const watchdogTimeoutMs = subagentWatchdogTimeoutMs();
    const watchdogPromise = watchdogTimeoutMs > 0 ? new Promise((resolve5) => {
      resolveWatchdog = resolve5;
    }) : null;
    const disarmWatchdog = () => {
      if (watchdogTimer) clearTimeout(watchdogTimer);
      watchdogTimer = null;
    };
    const armWatchdog = () => {
      if (!watchdogPromise || watchdogTimer || !acceptRuntimeEvents || activeSubagents.size === 0 && !workflowInFlight)
        return;
      watchdogTimer = setTimeout(() => {
        watchdogTimer = null;
        acceptRuntimeEvents = false;
        resolveWatchdog?.();
      }, watchdogTimeoutMs);
      watchdogTimer.unref();
    };
    const turnIsActive = () => acceptRuntimeEvents && this.store.getTurn(turn.id)?.status === "inProgress" && this.activeTurnByThread.get(thread.id) === turn.id;
    const runtimeTurn = this.runtime.runTurn(
      {
        threadId: thread.id,
        turnId: turn.id,
        purpose: turnPurpose,
        prompt: effectivePrompt,
        cwd: stringOr2(params.cwd, thread.cwd),
        runtimeType: isCodexThread ? "codex-proxy" : null,
        model: resolvedModel,
        effort: resolvedEffort,
        claudeSessionId: isCodexThread ? thread.codexSessionId : thread.claudeSessionId,
        forkSession,
        mcpServers: readMcpConfig().sdkValue,
        allowedTools: defaultAllowedTools(),
        addDirs: stringListFromEnv("CLAUDE_CODEX_ADD_DIRS", []),
        enableFileCheckpointing: process.env.CLAUDE_CODEX_ENABLE_FILE_CHECKPOINTING === "1",
        outputFormat: claudeOutputFormat(params.outputSchema),
        approvalPolicy,
        sandboxMode,
        systemPromptAddendum,
        planMode,
        imageInputs: Array.isArray(params._imageInputs) ? params._imageInputs : []
      },
      {
        onEvent: async (event) => {
          if (!turnIsActive()) return;
          if (event.type === "session") {
            this.store.updateClaudeSessionId(thread.id, event.claudeSessionId);
            return;
          }
          if (activeSubagents.size > 0) {
            if (event.type === "text_delta" || event.type === "reasoning_delta" || event.type === "message_boundary")
              return;
            if (event.type === "tool_use" && !isSubagentToolName(event.toolName)) return;
            if (event.type === "tool_output_delta" && !itemIds.has(event.toolUseId)) return;
            if (event.type === "tool_result" && !activeSubagents.has(event.toolUseId) && !itemIds.has(event.toolUseId))
              return;
          }
          if (event.type === "message_boundary") {
            completeMessage();
            return;
          }
          if (event.type === "tool_use" && !itemIds.has(event.toolUseId)) completeMessage();
          if (event.type === "tool_use" && isSubagentToolName(event.toolName)) {
            const promptText = String(event.input.prompt ?? event.input.description ?? "");
            const subType = typeof event.input.subagent_type === "string" ? event.input.subagent_type : null;
            const subagentModel = typeof event.input.model === "string" ? event.input.model : thread.model;
            const childThreadId = newId();
            const agentNickname = `agent-${childThreadId.replace(/-/g, "").slice(0, 12)}`;
            const agentPath = `/root/${agentNickname}`;
            const agentRole = subType ?? "general-purpose";
            const childStartedAt = nowSeconds();
            const childThread = {
              id: childThreadId,
              sessionId: thread.sessionId,
              forkedFromId: thread.id,
              isPinned: false,
              sectionId: null,
              sectionEnteredAt: null,
              sectionPosition: null,
              preview: promptText.slice(0, 200),
              name: null,
              archived: false,
              cwd: thread.cwd,
              model: subagentModel,
              reasoningEffort: thread.reasoningEffort,
              modelProvider: thread.modelProvider,
              claudeSessionId: null,
              source: normalizeSessionSource(thread.source),
              createdAt: childStartedAt,
              updatedAt: childStartedAt,
              status: { type: "active", activeFlags: [] },
              approvalPolicy: thread.approvalPolicy,
              sandboxMode: thread.sandboxMode,
              ephemeral: true,
              threadSource: "subagent",
              agentRole,
              agentNickname,
              // Subagent inherits parent's instruction surface so the same
              // project/developer guidance applies to the child run.
              baseInstructions: thread.baseInstructions,
              developerInstructions: thread.developerInstructions,
              personality: thread.personality,
              // Subagents always run via Claude — the Task tool is a Claude SDK
              // construct. A codex-backed thread that spawns a subagent would
              // never reach this code path (subagent detection is Claude-side).
              runtimeBackend: "claude",
              codexSessionId: null
            };
            this.store.upsertThread(childThread);
            this.activePeerByThread.set(childThreadId, peer);
            const childTurn = {
              id: newId(),
              threadId: childThreadId,
              status: "inProgress",
              startedAt: childStartedAt,
              completedAt: null,
              durationMs: null,
              items: [
                {
                  type: "userMessage",
                  id: newId(),
                  content: [{ type: "text", text: promptText, text_elements: [] }]
                }
              ],
              diff: "",
              error: null
            };
            this.store.upsertTurn(childTurn);
            this.notify(peer, {
              method: "thread/started",
              params: { thread: this.toThread(childThread, []) }
            });
            this.notify(peer, {
              method: "turn/started",
              params: {
                threadId: childThreadId,
                turn: this.toLifecycleTurn(childTurn)
              }
            });
            const childPrompt = childTurn.items.find((item) => item.type === "userMessage");
            if (childPrompt) this.emitItemLifecycle(peer, childThreadId, childTurn.id, childPrompt);
            recordRunEvent("subagent.spawned", {
              parentThreadId: thread.id,
              parentTurnId: turn.id,
              childThreadId,
              model: subagentModel,
              agentRole,
              agentNickname
            });
            const spawnId = newId();
            const collabEffort = normalizeReasoningEffortEnum(thread.reasoningEffort);
            const spawnBegin = {
              type: "collabAgentToolCall",
              id: spawnId,
              tool: "spawnAgent",
              status: "inProgress",
              senderThreadId: thread.id,
              receiverThreadIds: [],
              prompt: promptText || null,
              model: subagentModel,
              reasoningEffort: collabEffort,
              agentsStates: {}
            };
            this.store.appendItem(turn.id, spawnBegin);
            this.notify(peer, {
              method: "item/started",
              params: {
                threadId: thread.id,
                turnId: turn.id,
                item: spawnBegin,
                startedAtMs: nowMillis()
              }
            });
            const spawnEnd = {
              type: "collabAgentToolCall",
              id: spawnId,
              tool: "spawnAgent",
              status: "completed",
              senderThreadId: thread.id,
              receiverThreadIds: [childThreadId],
              prompt: promptText || null,
              model: subagentModel,
              reasoningEffort: collabEffort,
              agentsStates: { [childThreadId]: { status: "running", message: null } }
            };
            this.store.updateItem(turn.id, spawnId, () => spawnEnd);
            this.notify(peer, {
              method: "item/completed",
              params: {
                threadId: thread.id,
                turnId: turn.id,
                item: spawnEnd,
                completedAtMs: nowMillis()
              }
            });
            const waitId = newId();
            const subagentContext = {
              childThreadId,
              childTurnId: childTurn.id,
              waitItemId: waitId,
              agentPath,
              prompt: promptText,
              subType
            };
            this.emitSubagentActivity(peer, thread.id, turn.id, subagentContext, "started");
            const waitBegin = {
              type: "collabAgentToolCall",
              id: waitId,
              tool: "wait",
              status: "inProgress",
              senderThreadId: thread.id,
              receiverThreadIds: [childThreadId],
              prompt: null,
              model: null,
              reasoningEffort: null,
              agentsStates: {}
            };
            this.store.appendItem(turn.id, waitBegin);
            this.notify(peer, {
              method: "item/started",
              params: {
                threadId: thread.id,
                turnId: turn.id,
                item: waitBegin,
                startedAtMs: nowMillis()
              }
            });
            itemIds.set(event.toolUseId, waitId);
            subagentContexts.set(event.toolUseId, subagentContext);
            activeSubagents.add(event.toolUseId);
            armWatchdog();
            return;
          }
          if (event.type === "tool_result" && activeSubagents.has(event.toolUseId)) {
            const ctx = subagentContexts.get(event.toolUseId);
            if (!ctx) return;
            const rawResultText = toolResultText(event.content);
            const collabStatus = event.isError ? "failed" : "completed";
            const agentStatus = event.isError ? "errored" : "completed";
            const parsed = parseSubagentTrailer(rawResultText);
            const resultText2 = parsed.cleanText;
            const childTurn = this.completeSubagentChildTurn(
              peer,
              ctx,
              resultText2,
              event.isError ? "failed" : "completed",
              event.isError ? { message: "subagent failed" } : null,
              parsed.usage?.durationMs ?? null
            );
            recordRunEvent("subagent.completed", {
              parentThreadId: thread.id,
              parentTurnId: turn.id,
              childThreadId: ctx.childThreadId,
              childTurnId: childTurn.id,
              status: childTurn.status,
              agentRole: ctx.subType ?? "general-purpose"
            });
            const childThread = this.store.getThread(ctx.childThreadId);
            if (childThread) {
              childThread.updatedAt = nowSeconds();
              if (parsed.agentId) childThread.agentNickname = parsed.agentId;
              this.store.upsertThread(childThread);
            }
            if (event.isError || this.supportsCompletedSubagentActivity(peer)) {
              this.emitSubagentActivity(
                peer,
                thread.id,
                turn.id,
                ctx,
                event.isError ? "interrupted" : "completed"
              );
            }
            if (parsed.usage && parsed.usage.totalTokens) {
              const breakdown = {
                totalTokens: parsed.usage.totalTokens,
                inputTokens: 0,
                cachedInputTokens: 0,
                outputTokens: parsed.usage.totalTokens,
                reasoningOutputTokens: 0
              };
              const childUsage = {
                total: breakdown,
                last: breakdown,
                modelContextWindow: null
              };
              this.notify(peer, {
                method: "thread/tokenUsage/updated",
                params: {
                  threadId: ctx.childThreadId,
                  turnId: childTurn.id,
                  tokenUsage: childUsage
                }
              });
              this.recordTokenUsage(peer, thread.id, turn.id, {
                input_tokens: 0,
                output_tokens: parsed.usage.totalTokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0
              });
            }
            const waitEnd = {
              type: "collabAgentToolCall",
              id: ctx.waitItemId,
              tool: "wait",
              status: collabStatus,
              senderThreadId: thread.id,
              receiverThreadIds: [ctx.childThreadId],
              prompt: null,
              model: null,
              reasoningEffort: null,
              agentsStates: { [ctx.childThreadId]: { status: agentStatus, message: null } }
            };
            this.store.updateItemAndMoveToEnd(turn.id, ctx.waitItemId, () => waitEnd);
            this.notify(peer, {
              method: "item/completed",
              params: {
                threadId: thread.id,
                turnId: turn.id,
                item: waitEnd,
                completedAtMs: nowMillis()
              }
            });
            if (!this.supportsCompletedSubagentActivity(peer) && event.isError) {
              const closeId = newId();
              const closeBegin = {
                type: "collabAgentToolCall",
                id: closeId,
                tool: "closeAgent",
                status: "inProgress",
                senderThreadId: thread.id,
                receiverThreadIds: [ctx.childThreadId],
                prompt: null,
                model: null,
                reasoningEffort: null,
                agentsStates: {}
              };
              this.store.appendItem(turn.id, closeBegin);
              this.notify(peer, {
                method: "item/started",
                params: {
                  threadId: thread.id,
                  turnId: turn.id,
                  item: closeBegin,
                  startedAtMs: nowMillis()
                }
              });
              const closeEnd = {
                ...closeBegin,
                status: collabStatus,
                agentsStates: { [ctx.childThreadId]: { status: agentStatus, message: null } }
              };
              this.store.updateItem(turn.id, closeId, () => closeEnd);
              this.notify(peer, {
                method: "item/completed",
                params: {
                  threadId: thread.id,
                  turnId: turn.id,
                  item: closeEnd,
                  completedAtMs: nowMillis()
                }
              });
            }
            activeSubagents.delete(event.toolUseId);
            subagentContexts.delete(event.toolUseId);
            if (activeSubagents.size === 0 && !workflowInFlight) disarmWatchdog();
            return;
          }
          if (event.type === "text_delta") {
            if (event.delta.length === 0) return;
            completeReasoningItem();
            hasTextOutput = true;
            if (planMode) {
              const itemId2 = ensurePlanItem();
              this.store.updateItem(turn.id, itemId2, (item) => {
                if (item.type === "plan") return { ...item, text: item.text + event.delta };
                return item;
              });
              this.notify(peer, {
                method: "item/plan/delta",
                params: { threadId: thread.id, turnId: turn.id, itemId: itemId2, delta: event.delta }
              });
              return;
            }
            const itemId = ensureAgentItem();
            this.store.updateItem(turn.id, itemId, (item) => {
              if (item.type === "agentMessage") return { ...item, text: item.text + event.delta };
              return item;
            });
            this.notify(peer, {
              method: "item/agentMessage/delta",
              params: { threadId: thread.id, turnId: turn.id, itemId, delta: event.delta }
            });
            return;
          }
          if (event.type === "reasoning_delta") {
            if (event.delta.length === 0) return;
            completeAgentItem("commentary");
            const itemId = ensureReasoningItem();
            this.store.updateItem(turn.id, itemId, (item) => {
              if (item.type === "reasoning") {
                return {
                  ...item,
                  content: [(item.content[0] ?? "") + event.delta]
                };
              }
              return item;
            });
            this.notify(peer, {
              method: "item/reasoning/textDelta",
              params: {
                threadId: thread.id,
                turnId: turn.id,
                itemId,
                delta: event.delta,
                contentIndex: 0
              }
            });
            return;
          }
          if (event.type === "tool_use") {
            if (isWorkflowToolName(event.toolName)) {
              workflowLaunchToolUseIds.add(event.toolUseId);
              workflowInFlight = true;
              armWatchdog();
            }
            if (itemIds.has(event.toolUseId)) return;
            if (event.toolName === "TodoWrite") {
              const plan = todoWriteToPlanSteps(event.input);
              if (plan) {
                this.notify(peer, {
                  method: "turn/plan/updated",
                  params: { threadId: thread.id, turnId: turn.id, explanation: null, plan }
                });
              }
              itemIds.set(event.toolUseId, "");
              return;
            }
            const item = this.toolUseToItem(event, thread.cwd);
            itemIds.set(event.toolUseId, item.id);
            itemStartedAtMs.set(item.id, nowMillis());
            this.store.appendItem(turn.id, item);
            this.notify(peer, {
              method: "item/started",
              params: { threadId: thread.id, turnId: turn.id, item, startedAtMs: nowMillis() }
            });
            if (item.type === "fileChange") {
              this.notify(peer, {
                method: "item/fileChange/patchUpdated",
                params: {
                  threadId: thread.id,
                  turnId: turn.id,
                  itemId: item.id,
                  changes: item.changes
                }
              });
            }
            return;
          }
          if (event.type === "tool_output_delta") {
            const itemId = itemIds.get(event.toolUseId);
            if (!itemId) return;
            commandOutputSeen.add(itemId);
            this.store.updateItem(turn.id, itemId, (item) => {
              if (item.type === "commandExecution") {
                return { ...item, aggregatedOutput: `${item.aggregatedOutput ?? ""}${event.delta}` };
              }
              return item;
            });
            this.notify(peer, {
              method: "item/commandExecution/outputDelta",
              params: { threadId: thread.id, turnId: turn.id, itemId, delta: event.delta }
            });
            return;
          }
          if (event.type === "tool_result") {
            if (workflowLaunchToolUseIds.delete(event.toolUseId)) {
              workflowInFlight = workflowLaunchToolUseIds.size > 0;
              if (activeSubagents.size === 0 && !workflowInFlight) disarmWatchdog();
            }
            const itemId = itemIds.get(event.toolUseId);
            if (!itemId) return;
            const resultText2 = toolResultText(event.content);
            const durationMs = (() => {
              const started = itemStartedAtMs.get(itemId);
              return started == null ? null : Math.max(0, nowMillis() - started);
            })();
            const parsedExitCode = parseExitCodeFromResult(event.content) ?? (event.isError ? 1 : 0);
            const updated = this.store.updateItem(turn.id, itemId, (item2) => {
              if (item2.type === "commandExecution") {
                return {
                  ...item2,
                  status: event.isError ? "failed" : "completed",
                  aggregatedOutput: item2.aggregatedOutput ?? resultText2,
                  exitCode: parsedExitCode,
                  durationMs
                };
              }
              if (item2.type === "fileChange")
                return { ...item2, status: event.isError ? "failed" : "completed" };
              if (item2.type === "mcpToolCall") {
                return {
                  ...item2,
                  status: event.isError ? "failed" : "completed",
                  result: event.isError ? null : wrapMcpToolResult(event.content),
                  error: event.isError ? wrapMcpToolError(event.content) : null,
                  durationMs
                };
              }
              if (item2.type === "webSearch") {
                return { ...item2, action: parseWebSearchAction(item2.query, resultText2) };
              }
              return item2;
            });
            const item = updated?.items.find((candidate) => candidate.id === itemId);
            if (item?.type === "commandExecution" && resultText2 && !commandOutputSeen.has(itemId)) {
              this.notify(peer, {
                method: "item/commandExecution/outputDelta",
                params: { threadId: thread.id, turnId: turn.id, itemId, delta: resultText2 }
              });
            }
            if (item)
              this.notify(peer, {
                method: "item/completed",
                params: { threadId: thread.id, turnId: turn.id, item, completedAtMs: nowMillis() }
              });
            const diff = await gitDiff(thread.cwd);
            if (this.store.getTurn(turn.id)?.status !== "inProgress") return;
            if (diff) {
              this.store.updateTurnDiff(turn.id, diff);
              this.notify(peer, {
                method: "turn/diff/updated",
                params: { threadId: thread.id, turnId: turn.id, diff }
              });
            }
            return;
          }
          if (event.type === "notice") {
            debugLog("turn.runtime.notice", { threadId: thread.id, turnId: turn.id, ...event });
            if (event.level !== "info" && !noticesSeen.has(event.message)) {
              noticesSeen.add(event.message);
              this.notify(peer, {
                method: "warning",
                params: { threadId: thread.id, message: event.message }
              });
            }
            return;
          }
          if (event.type === "usage") {
            this.recordTokenUsage(peer, thread.id, turn.id, event.usage);
            return;
          }
          if (event.type === "rate_limit") {
            this.rateLimits.recordEvent(event.info);
            return;
          }
          if (event.type === "hook") {
            const hookRunId = newId();
            const fragments = [
              { text: `Hook \xB7 ${event.hookName}`, hookRunId }
            ];
            if (event.status) fragments.push({ text: `status: ${event.status}`, hookRunId });
            if (event.decision) fragments.push({ text: `decision: ${event.decision}`, hookRunId });
            if (event.message) fragments.push({ text: event.message, hookRunId });
            const hookItem = { type: "hookPrompt", id: newId(), fragments };
            this.store.appendItem(turn.id, hookItem);
            this.notify(peer, {
              method: "item/started",
              params: {
                threadId: thread.id,
                turnId: turn.id,
                item: hookItem,
                startedAtMs: nowMillis()
              }
            });
            this.notify(peer, {
              method: "item/completed",
              params: {
                threadId: thread.id,
                turnId: turn.id,
                item: hookItem,
                completedAtMs: nowMillis()
              }
            });
            return;
          }
          if (event.type === "metrics") {
            collectedMetrics.apiDurationMs = event.apiDurationMs;
            collectedMetrics.numTurns = event.numTurns;
            collectedMetrics.costUsd = event.costUsd;
            collectedMetrics.set = true;
            return;
          }
          if (event.type === "completed") {
            if (event.claudeSessionId) {
              if (isCodexThread) {
                this.store.updateCodexSessionId(thread.id, event.claudeSessionId);
              } else {
                this.store.updateClaudeSessionId(thread.id, event.claudeSessionId);
              }
            }
            if (!event.success) throw new Error(event.result ?? "Claude turn failed");
          }
          if (event.type === "error") {
            throw new Error(event.message);
          }
        },
        onPermissionRequest: async (event) => {
          if (!turnIsActive()) return { decision: "cancel" };
          completeMessage();
          let itemId = itemIds.get(event.toolUseId);
          if (!itemId) {
            const item = this.toolUseToItem(
              {
                type: "tool_use",
                toolUseId: event.toolUseId,
                toolName: event.toolName,
                input: event.input
              },
              thread.cwd
            );
            itemId = item.id;
            itemIds.set(event.toolUseId, item.id);
            this.store.appendItem(turn.id, item);
            this.notify(peer, {
              method: "item/started",
              params: { threadId: thread.id, turnId: turn.id, item, startedAtMs: nowMillis() }
            });
            if (item.type === "fileChange") {
              this.notify(peer, {
                method: "item/fileChange/patchUpdated",
                params: {
                  threadId: thread.id,
                  turnId: turn.id,
                  itemId: item.id,
                  changes: item.changes
                }
              });
            }
          }
          const decision = await this.requestApproval(peer, thread.id, turn.id, itemId, event);
          if (!turnIsActive()) return { decision: "cancel" };
          if (decision.decision === "acceptForSession") {
            const command = String(event.input.command ?? "");
            if (command) {
              const set = this.commandSessionAllow.get(thread.id) ?? /* @__PURE__ */ new Set();
              set.add(command);
              this.commandSessionAllow.set(thread.id, set);
            }
          }
          return decision;
        },
        onUserInputRequest: async (event) => {
          if (!turnIsActive()) return { answers: {} };
          completeMessage();
          const item = {
            type: "dynamicToolCall",
            id: newId(),
            namespace: "claude",
            tool: "AskUserQuestion",
            arguments: { questions: event.questions },
            status: "inProgress",
            contentItems: null,
            success: null,
            durationMs: null
          };
          this.store.appendItem(turn.id, item);
          const startedAt = nowMillis();
          this.notify(peer, {
            method: "item/started",
            params: { threadId: thread.id, turnId: turn.id, item, startedAtMs: startedAt }
          });
          this.setThreadStatus(peer, thread.id, {
            type: "active",
            activeFlags: ["waitingOnUserInput"]
          });
          const answers = await this.requestUserInput(
            peer,
            thread.id,
            turn.id,
            item.id,
            event.questions
          );
          if (!turnIsActive()) return { answers: {} };
          const contentItems = userInputAnswersAsContent(event.questions, answers);
          const completedItem = {
            ...item,
            status: "completed",
            success: true,
            contentItems,
            durationMs: Math.max(0, nowMillis() - startedAt)
          };
          this.store.updateItem(turn.id, item.id, () => completedItem);
          this.notify(peer, {
            method: "item/completed",
            params: {
              threadId: thread.id,
              turnId: turn.id,
              item: completedItem,
              completedAtMs: nowMillis()
            }
          });
          this.setThreadStatus(peer, thread.id, { type: "active", activeFlags: [] });
          return answers;
        }
      }
    );
    const runtimeOutcome = runtimeTurn.then(
      () => ({ kind: "resolved" }),
      (error) => ({
        kind: "rejected",
        error: error instanceof Error ? error : new Error(String(error))
      })
    );
    const outcome = watchdogPromise ? await Promise.race([
      runtimeOutcome,
      watchdogPromise.then(() => ({ kind: "timeout" }))
    ]) : await runtimeOutcome;
    disarmWatchdog();
    acceptRuntimeEvents = false;
    if (outcome.kind === "timeout") {
      const timeoutSeconds = Math.ceil(watchdogTimeoutMs / 1e3);
      const timeoutUnit = timeoutSeconds === 1 ? "second" : "seconds";
      const message = `Subagent did not publish a terminal result within ${timeoutSeconds} ${timeoutUnit}.`;
      if (this.store.getTurn(turn.id)?.status === "inProgress") {
        completeMessage();
        this.finalizeOrphanedSubagents(
          peer,
          thread,
          turn,
          subagentContexts,
          activeSubagents,
          message
        );
        this.subagentStateByTurn.delete(turn.id);
        void this.runtime.interrupt(thread.id).catch(() => {
        });
      }
      throw new Error(message);
    }
    if (outcome.kind === "rejected") {
      if (this.store.getTurn(turn.id)?.status === "inProgress") {
        completeMessage();
        this.finalizeOrphanedSubagents(peer, thread, turn, subagentContexts, activeSubagents);
      }
      this.subagentStateByTurn.delete(turn.id);
      throw outcome.error;
    }
    if (this.store.getTurn(turn.id)?.status === "inProgress") {
      this.finalizeOrphanedSubagents(peer, thread, turn, subagentContexts, activeSubagents);
    }
    this.subagentStateByTurn.delete(turn.id);
    const currentTurn = this.store.getTurn(turn.id);
    if (currentTurn && currentTurn.status !== "inProgress") return;
    const finalDiff = await gitDiff(thread.cwd);
    if (this.store.getTurn(turn.id)?.status !== "inProgress") return;
    if (finalDiff) {
      this.store.updateTurnDiff(turn.id, finalDiff);
      this.notify(peer, {
        method: "turn/diff/updated",
        params: { threadId: thread.id, turnId: turn.id, diff: finalDiff }
      });
    }
    if (params.outputSchema != null && !hasTextOutput) {
      const text = fallbackStructuredText(params.outputSchema, prompt);
      const itemId = ensureAgentItem();
      this.store.updateItem(turn.id, itemId, (item) => {
        if (item.type === "agentMessage") return { ...item, text };
        return item;
      });
      this.notify(peer, {
        method: "item/agentMessage/delta",
        params: { threadId: thread.id, turnId: turn.id, itemId, delta: text }
      });
    }
    completeMessage("final_answer");
    const planItem = this.store.getTurn(turn.id)?.items.find((item) => item.id === planItemId);
    if (planItem)
      this.notify(peer, {
        method: "item/completed",
        params: {
          threadId: thread.id,
          turnId: turn.id,
          item: planItem,
          completedAtMs: nowMillis()
        }
      });
    const completed = this.store.completeTurn(turn.id, "completed") ?? turn;
    recordRunEvent("turn.completed", {
      threadId: thread.id,
      turnId: turn.id,
      runtimeBackend: thread.runtimeBackend,
      durationMs: completed.durationMs,
      apiDurationMs: collectedMetrics.apiDurationMs,
      numTurns: collectedMetrics.numTurns,
      costUsd: collectedMetrics.costUsd
    });
    if (collectedMetrics.set) {
      completed.apiDurationMs = collectedMetrics.apiDurationMs;
      completed.numTurns = collectedMetrics.numTurns;
      completed.costUsd = collectedMetrics.costUsd;
    }
    this.clearActiveTurn(thread.id);
    this.setThreadStatus(peer, thread.id, { type: "idle" });
    this.notify(peer, {
      method: "turn/completed",
      params: { threadId: thread.id, turn: this.toLifecycleTurn(completed) }
    });
  }
  completeSubagentChildTurn(peer, context, resultText2, status, error, durationMs = null) {
    let childTurn = this.store.getTurn(context.childTurnId);
    if (!childTurn) {
      childTurn = {
        id: context.childTurnId,
        threadId: context.childThreadId,
        status: "inProgress",
        startedAt: nowSeconds(),
        completedAt: null,
        durationMs: null,
        items: [
          {
            type: "userMessage",
            id: newId(),
            content: [{ type: "text", text: context.prompt, text_elements: [] }]
          }
        ],
        diff: "",
        error: null
      };
      this.store.upsertTurn(childTurn);
    }
    const agentItemId = newId();
    const startedItem = {
      type: "agentMessage",
      id: agentItemId,
      text: "",
      phase: null,
      memoryCitation: null
    };
    this.store.appendItem(childTurn.id, startedItem);
    this.notify(peer, {
      method: "item/started",
      params: {
        threadId: context.childThreadId,
        turnId: childTurn.id,
        item: startedItem,
        startedAtMs: nowMillis()
      }
    });
    const completedItem = { ...startedItem, text: resultText2 };
    this.store.updateItem(childTurn.id, agentItemId, () => completedItem);
    if (resultText2) {
      this.notify(peer, {
        method: "item/agentMessage/delta",
        params: {
          threadId: context.childThreadId,
          turnId: childTurn.id,
          itemId: agentItemId,
          delta: resultText2
        }
      });
    }
    this.notify(peer, {
      method: "item/completed",
      params: {
        threadId: context.childThreadId,
        turnId: childTurn.id,
        item: completedItem,
        completedAtMs: nowMillis()
      }
    });
    const completed = this.store.completeTurn(childTurn.id, status, error) ?? childTurn;
    if (durationMs != null) {
      completed.durationMs = durationMs;
      this.store.upsertTurn(completed);
    }
    this.setThreadStatus(peer, context.childThreadId, { type: "idle" });
    this.notify(peer, {
      method: "turn/completed",
      params: {
        threadId: context.childThreadId,
        turn: this.toCompletedTurn(completed)
      }
    });
    return completed;
  }
  emitSubagentActivity(peer, parentThreadId, parentTurnId, context, kind) {
    if (!this.supportsCompletedSubagentActivity(peer) && kind !== "interrupted") return;
    const item = {
      type: "subAgentActivity",
      id: newId(),
      kind,
      agentThreadId: context.childThreadId,
      agentPath: context.agentPath
    };
    if (kind === "interrupted") this.store.appendItem(parentTurnId, item);
    this.emitItemLifecycle(peer, parentThreadId, parentTurnId, item);
  }
  emitItemLifecycle(peer, threadId, turnId, item) {
    const startedAtMs = nowMillis();
    this.notify(peer, {
      method: "item/started",
      params: {
        threadId,
        turnId,
        item,
        startedAtMs
      }
    });
    this.notify(peer, {
      method: "item/completed",
      params: {
        threadId,
        turnId,
        item,
        completedAtMs: nowMillis()
      }
    });
  }
  supportsCompletedSubagentActivity(peer) {
    if (process.env.CLAUDE_CODEX_SUBAGENT_COMPLETED === "1") return true;
    if (process.env.CLAUDE_CODEX_SUBAGENT_COMPLETED === "0") return false;
    return this.peerFeatures.get(peer)?.supportsCompletedSubagentActivity === true;
  }
  finalizeOrphanedSubagents(peer, thread, turn, contexts, active, failureMessage = "Subagent ended without a terminal task result.") {
    for (const toolUseId of Array.from(active)) {
      const context = contexts.get(toolUseId);
      active.delete(toolUseId);
      contexts.delete(toolUseId);
      if (!context) continue;
      const message = failureMessage;
      const childTurn = this.completeSubagentChildTurn(peer, context, message, "failed", {
        message
      });
      recordRunEvent("subagent.completed", {
        parentThreadId: thread.id,
        parentTurnId: turn.id,
        childThreadId: context.childThreadId,
        childTurnId: childTurn.id,
        status: "failed",
        agentRole: context.subType ?? "general-purpose"
      });
      this.emitSubagentActivity(peer, thread.id, turn.id, context, "interrupted");
      const waitEnd = {
        type: "collabAgentToolCall",
        id: context.waitItemId,
        tool: "wait",
        status: "failed",
        senderThreadId: thread.id,
        receiverThreadIds: [context.childThreadId],
        prompt: null,
        model: null,
        reasoningEffort: null,
        agentsStates: { [context.childThreadId]: { status: "errored", message } }
      };
      this.store.updateItemAndMoveToEnd(turn.id, context.waitItemId, () => waitEnd);
      this.notify(peer, {
        method: "item/completed",
        params: {
          threadId: thread.id,
          turnId: turn.id,
          item: waitEnd,
          completedAtMs: nowMillis()
        }
      });
      if (!this.supportsCompletedSubagentActivity(peer)) {
        const closeId = newId();
        const closeBegin = {
          type: "collabAgentToolCall",
          id: closeId,
          tool: "closeAgent",
          status: "inProgress",
          senderThreadId: thread.id,
          receiverThreadIds: [context.childThreadId],
          prompt: null,
          model: null,
          reasoningEffort: null,
          agentsStates: {}
        };
        this.store.appendItem(turn.id, closeBegin);
        this.notify(peer, {
          method: "item/started",
          params: {
            threadId: thread.id,
            turnId: turn.id,
            item: closeBegin,
            startedAtMs: nowMillis()
          }
        });
        const closeEnd = {
          ...closeBegin,
          status: "failed",
          agentsStates: { [context.childThreadId]: { status: "errored", message } }
        };
        this.store.updateItem(turn.id, closeId, () => closeEnd);
        this.notify(peer, {
          method: "item/completed",
          params: {
            threadId: thread.id,
            turnId: turn.id,
            item: closeEnd,
            completedAtMs: nowMillis()
          }
        });
      }
    }
  }
  async requestApproval(peer, threadId, turnId, itemId, event) {
    const thread = this.store.getThread(threadId);
    if (thread && (thread.approvalPolicy === "never" || thread.sandboxMode === "danger-full-access")) {
      return { decision: "accept" };
    }
    const command = String(event.input.command ?? "");
    if (command && this.commandSessionAllow.get(threadId)?.has(command)) {
      return { decision: "accept" };
    }
    this.setThreadStatus(peer, threadId, { type: "active", activeFlags: ["waitingOnApproval"] });
    const requestId = newId();
    const isCommand = event.toolName === "Bash";
    const method = isCommand ? "item/commandExecution/requestApproval" : "item/fileChange/requestApproval";
    const params = isCommand ? {
      threadId,
      turnId,
      itemId,
      startedAtMs: nowMillis(),
      approvalId: requestId,
      reason: null,
      command,
      cwd: String(event.input.cwd ?? ""),
      commandActions: [],
      additionalPermissions: null,
      proposedExecpolicyAmendment: null,
      proposedNetworkPolicyAmendments: null,
      availableDecisions: ["accept", "acceptForSession", "decline", "cancel"]
    } : {
      threadId,
      turnId,
      itemId,
      startedAtMs: nowMillis(),
      reason: null,
      grantRoot: null
    };
    let response;
    try {
      response = await this.sendServerRequest(peer, method, requestId, params);
    } finally {
      this.notify(peer, { method: "serverRequest/resolved", params: { threadId, requestId } });
      this.setThreadStatus(peer, threadId, { type: "active", activeFlags: [] });
    }
    const decision = normalizeDecision(response);
    return { decision };
  }
  async requestUserInput(peer, threadId, turnId, itemId, questions) {
    const requestId = newId();
    const normalized = questions.map((q) => {
      const options = q.options ?? [];
      const hasOther = options.some((o) => o.label === "Other");
      return hasOther ? q : {
        ...q,
        options: [...options, { label: "Other", description: "Provide a free-form answer" }]
      };
    });
    const params = { threadId, turnId, itemId, questions: normalized, isBlocking: true };
    let response;
    try {
      response = await this.sendServerRequest(peer, "item/tool/requestUserInput", requestId, params);
    } finally {
      this.notify(peer, { method: "serverRequest/resolved", params: { threadId, requestId } });
    }
    return normalizeUserInputAnswers(response, normalized);
  }
  async turnInterrupt(peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const requestedTurnId = stringOr2(params.turnId, "");
    this.activePeerByThread.set(threadId, peer);
    const activeTurnId = this.activeTurnByThread.get(threadId);
    const turnId = activeTurnId || requestedTurnId;
    if (turnId) {
      const turn = this.store.getTurn(turnId);
      if (turn?.status === "inProgress") {
        const subagents = this.subagentStateByTurn.get(turnId);
        if (subagents) {
          this.finalizeOrphanedSubagents(
            peer,
            subagents.thread,
            subagents.turn,
            subagents.contexts,
            subagents.active
          );
          this.subagentStateByTurn.delete(turnId);
        }
        const completed = this.store.completeTurn(turnId, "interrupted", { message: "interrupted" }) ?? turn;
        this.notify(peer, {
          method: "turn/completed",
          params: { threadId, turn: this.toLifecycleTurn(completed) }
        });
      }
    }
    this.clearActiveTurn(threadId);
    this.setThreadStatus(peer, threadId, { type: "idle" });
    const interrupting = this.runtime.interrupt(threadId);
    await interrupting;
    return {};
  }
  async turnSteer(_peer, params) {
    const threadId = stringOr2(params.threadId, "");
    const expectedTurnId = stringOr2(params.expectedTurnId, "");
    const activeTurnId = this.activeTurnByThread.get(threadId);
    if (!activeTurnId) throw new Error(`thread has no active turn: ${threadId}`);
    if (expectedTurnId && expectedTurnId !== activeTurnId) {
      throw new Error(`active turn mismatch: expected ${expectedTurnId}, got ${activeTurnId}`);
    }
    const input = Array.isArray(params.input) ? params.input : [];
    const prompt = textFromInput(input);
    const item = { type: "userMessage", id: newId(), content: input };
    this.store.appendItem(activeTurnId, item);
    await this.runtime.steer(threadId, prompt);
    return { turnId: activeTurnId };
  }
  configRead() {
    return {
      config: {
        ...this.publicConfigOverrides(),
        model: this.configModel,
        review_model: null,
        model_context_window: null,
        model_auto_compact_token_limit: null,
        model_provider: "claude-code",
        approval_policy: this.configOverrides.approval_policy ?? "on-request",
        approvals_reviewer: this.configOverrides.approvals_reviewer ?? "user",
        sandbox_mode: this.configOverrides.sandbox_mode ?? "workspace-write",
        sandbox_workspace_write: this.configOverrides.sandbox_workspace_write ?? null,
        forced_chatgpt_workspace_id: null,
        forced_login_method: null,
        web_search: this.configOverrides.web_search ?? "disabled",
        tools: this.configOverrides.tools ?? null,
        profile: this.configOverrides.profile ?? null,
        profiles: {},
        instructions: this.configOverrides.instructions ?? null,
        developer_instructions: this.configOverrides.developer_instructions ?? null,
        compact_prompt: this.configOverrides.compact_prompt ?? null,
        model_reasoning_effort: this.configReasoningEffort,
        model_reasoning_summary: this.configOverrides.model_reasoning_summary ?? null,
        model_verbosity: this.configOverrides.model_verbosity ?? null,
        service_tier: this.configOverrides.service_tier ?? null,
        analytics: this.configOverrides.analytics ?? null,
        apps: this.configOverrides.apps ?? null,
        model_providers: this.exposedModelProviders(),
        provider_loop_config: projectProviderLoopConfig(
          void 0,
          this.providerLoopSelectionInput()
        )
      },
      origins: {
        model_provider: configLayerMetadata(),
        "model_providers.claude-code": configLayerMetadata(),
        ...codexProxyModelOptions().length > 0 ? { "model_providers.codex": configLayerMetadata() } : {}
      },
      layers: null
    };
  }
  providerLoopSelectionInput() {
    const legacyRuntimeType = normalizeRuntimeType(
      process.env.CLAUDE_CODEX_RUNTIME_TYPE ?? process.env.CLAUDE_CODEX_RUNTIME ?? process.env.CLAUDE_CODEX_BACKEND
    );
    const envInput = providerLoopSelectionInputFromEnv(process.env, legacyRuntimeType);
    if (hasProviderLoopSelectionInput(envInput)) return envInput;
    return providerLoopSelectionInputFromConfig(
      this.configOverrides,
      legacyRuntimeType,
      process.env.CLAUDE_CODEX_MOCK === "1"
    );
  }
  publicConfigOverrides() {
    return Object.fromEntries(
      Object.entries(this.configOverrides).filter(
        ([key]) => !isProviderLoopSelectionConfigKey(key)
      )
    );
  }
  // Build the model_providers map served by config/read. Always exposes
  // 'claude-code'; conditionally adds 'codex' when a real Codex CLI is
  // resolvable on the host. Exposing 'codex' as a separate provider entry
  // gives the App's settings panel a way to surface OpenAI-family models
  // (gpt-*) without the App treating them as foreign to the Claude
  // provider's allowlist.
  exposedModelProviders() {
    const providers = {
      "claude-code": {
        name: "Claude Code",
        base_url: null,
        env_key: null,
        env_key_instructions: null,
        experimental_bearer_token: null,
        auth: null,
        aws: null,
        wire_api: "responses",
        query_params: null,
        http_headers: null,
        env_http_headers: null,
        request_max_retries: null,
        stream_max_retries: null,
        stream_idle_timeout_ms: null,
        websocket_connect_timeout_ms: null,
        requires_openai_auth: false,
        supports_websockets: false
      }
    };
    if (codexProxyModelOptions().length > 0) {
      providers.codex = {
        name: "Codex (OpenAI \xB7 forwarded)",
        base_url: null,
        env_key: null,
        env_key_instructions: null,
        experimental_bearer_token: null,
        auth: null,
        aws: null,
        wire_api: "responses",
        query_params: null,
        http_headers: null,
        env_http_headers: null,
        request_max_retries: null,
        stream_max_retries: null,
        stream_idle_timeout_ms: null,
        websocket_connect_timeout_ms: null,
        requires_openai_auth: false,
        supports_websockets: false
      };
    }
    return providers;
  }
  async modelList() {
    const models = await this.runtime.supportedModels?.();
    if (models) return runtimeModelList(models, this.configModel, this.configReasoningEffort);
    const defaultModel = this.configModel;
    const claudeOptions = claudeModelOptions();
    const codexOptions = codexProxyModelOptions();
    const options = [...claudeOptions, ...codexOptions];
    const hasConfiguredDefault = options.some((option) => option.id === defaultModel);
    const reasoningEfforts = [
      { reasoningEffort: "low", description: "Fast runtime response" },
      { reasoningEffort: "medium", description: "Balanced runtime response" },
      { reasoningEffort: "high", description: "Deeper runtime response" },
      { reasoningEffort: "xhigh", description: "Maximum reasoning" }
    ];
    return {
      data: options.map((option) => ({
        id: option.id,
        model: option.id,
        upgrade: null,
        upgradeInfo: null,
        availabilityNux: null,
        displayName: option.displayName,
        description: option.description,
        hidden: false,
        supportedReasoningEfforts: reasoningEfforts,
        defaultReasoningEffort: this.configReasoningEffort,
        inputModalities: ["text", "image"],
        supportsPersonality: false,
        additionalSpeedTiers: [],
        isDefault: hasConfiguredDefault ? option.id === defaultModel : option.isDefault === true
      })),
      nextCursor: null
    };
  }
  // Serves the latest known windows. A read refreshes them from `/usage` at most once a minute;
  // when that fails, turn events and the retained snapshot still answer.
  async accountRateLimits() {
    if (this.runtime.planUsage && this.rateLimits.usageDue()) {
      this.rateLimitRefresh ??= this.runtime.planUsage().then((limits) => this.rateLimits.recordUsage(limits)).catch((error) => {
        this.rateLimits.recordUsage(void 0);
        debugLog("account.rateLimits.refreshFailed", { message: String(error) });
      }).finally(() => {
        this.rateLimitRefresh = null;
      });
      await this.rateLimitRefresh;
    }
    return this.rateLimits.read();
  }
  // Accumulates Claude Agent SDK token usage per thread and pushes a
  // `thread/tokenUsage/updated` notification so the Codex App can render real
  // consumption instead of leaving the meter blank.
  recordTokenUsage(peer, threadId, turnId, usage) {
    const last = tokenBreakdownFromClaudeUsage(usage);
    if (last.totalTokens === 0) return;
    const prior = this.tokenUsageByThread.get(threadId) ?? emptyTokenBreakdown();
    const total = {
      totalTokens: prior.totalTokens + last.totalTokens,
      inputTokens: prior.inputTokens + last.inputTokens,
      cachedInputTokens: prior.cachedInputTokens + last.cachedInputTokens,
      outputTokens: prior.outputTokens + last.outputTokens,
      reasoningOutputTokens: prior.reasoningOutputTokens + last.reasoningOutputTokens
    };
    this.tokenUsageByThread.set(threadId, total);
    const tokenUsage = { total, last, modelContextWindow: null };
    this.notify(peer, {
      method: "thread/tokenUsage/updated",
      params: { threadId, turnId, tokenUsage }
    });
  }
  async fsReadFile(params) {
    const { readFile } = await import("node:fs/promises");
    const path = stringOr2(params.path ?? params.filePath, "");
    return { dataBase64: (await readFile(path)).toString("base64") };
  }
  async fsReadDirectory(params) {
    const { readdir } = await import("node:fs/promises");
    const path = stringOr2(params.path, process.cwd());
    const entries = await readdir(path, { withFileTypes: true });
    return {
      entries: entries.map((entry) => ({
        fileName: entry.name,
        isDirectory: entry.isDirectory(),
        isFile: entry.isFile()
      }))
    };
  }
  async fsGetMetadata(params) {
    const { stat } = await import("node:fs/promises");
    const path = stringOr2(params.path, "");
    const metadata2 = await stat(path);
    return {
      isDirectory: metadata2.isDirectory(),
      isFile: metadata2.isFile(),
      isSymlink: metadata2.isSymbolicLink(),
      createdAtMs: metadata2.birthtimeMs,
      modifiedAtMs: metadata2.mtimeMs
    };
  }
  async fsWriteFile(params) {
    const { writeFile } = await import("node:fs/promises");
    const path = stringOr2(params.path, "");
    const data = typeof params.dataBase64 === "string" ? Buffer.from(params.dataBase64, "base64") : Buffer.alloc(0);
    await writeFile(path, data);
    return {};
  }
  async fsCreateDirectory(params) {
    const { mkdir } = await import("node:fs/promises");
    await mkdir(stringOr2(params.path, ""), { recursive: params.recursive !== false });
    return {};
  }
  async fsRemove(params) {
    const { rm } = await import("node:fs/promises");
    await rm(stringOr2(params.path, ""), {
      recursive: params.recursive !== false,
      force: params.force !== false
    });
    return {};
  }
  async fsCopy(params) {
    const { cp } = await import("node:fs/promises");
    await cp(stringOr2(params.sourcePath, ""), stringOr2(params.destinationPath, ""), {
      recursive: params.recursive === true
    });
    return {};
  }
  async fsWatch(peer, params) {
    const { realpath: realpath2 } = await import("node:fs/promises");
    const watchId = stringOr2(params.watchId, newId());
    const path = await realpath2(stringOr2(params.path, process.cwd()));
    this.fsWatchers.get(watchId)?.close();
    const watcher = watch(path, { persistent: false }, (_eventType, filename) => {
      const changedPath = filename ? `${path}/${String(filename)}` : path;
      this.notify(peer, { method: "fs/changed", params: { watchId, changedPaths: [changedPath] } });
    });
    this.fsWatchers.set(watchId, watcher);
    return { path };
  }
  fsUnwatch(params) {
    const watchId = stringOr2(params.watchId, "");
    this.fsWatchers.get(watchId)?.close();
    this.fsWatchers.delete(watchId);
    return {};
  }
  async commandExec(peer, params) {
    const processId = typeof params.processId === "string" ? params.processId : newId();
    const command = commandArray(params.command);
    if (command.length === 0) throw new Error("command/exec requires command");
    if ((params.streamStdoutStderr === true || params.streamStdin === true || params.tty === true) && typeof params.processId !== "string") {
      throw new Error("command/exec streaming requires processId");
    }
    const executable = command[0];
    const streamOutput = params.streamStdoutStderr === true || params.tty === true;
    const cwd = stringOr2(params.cwd, process.cwd());
    debugLog("command.exec.start", {
      processId,
      cwd,
      command,
      streamOutput,
      streamStdin: params.streamStdin === true,
      tty: params.tty === true
    });
    const child = spawn3(executable, command.slice(1), {
      cwd,
      env: commandEnv(params.env),
      stdio: "pipe"
    });
    this.commandProcesses.set(processId, child);
    const stdout = [];
    const stderr = [];
    const cap = params.disableOutputCap === true ? Number.POSITIVE_INFINITY : numberOr(params.outputBytesCap, 1e6);
    let stdoutBytes = 0;
    let stderrBytes = 0;
    const capture = (target, chunk, currentBytes) => {
      if (currentBytes >= cap) return currentBytes;
      const allowed = Math.min(chunk.byteLength, cap - currentBytes);
      if (allowed > 0) target.push(chunk.subarray(0, allowed));
      return currentBytes + allowed;
    };
    child.stdout?.on("data", (chunk) => {
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      if (streamOutput) {
        this.notify(peer, {
          method: "command/exec/outputDelta",
          params: {
            processId,
            stream: "stdout",
            deltaBase64: buffer.toString("base64"),
            capReached: false
          }
        });
        return;
      }
      stdoutBytes = capture(stdout, buffer, stdoutBytes);
    });
    child.stderr?.on("data", (chunk) => {
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      if (streamOutput) {
        this.notify(peer, {
          method: "command/exec/outputDelta",
          params: {
            processId,
            stream: "stderr",
            deltaBase64: buffer.toString("base64"),
            capReached: false
          }
        });
        return;
      }
      stderrBytes = capture(stderr, buffer, stderrBytes);
    });
    const timeoutMs = params.disableTimeout === true ? null : numberOr(params.timeoutMs, 6e4);
    let timeout = null;
    if (timeoutMs != null && timeoutMs > 0) {
      timeout = setTimeout(() => child.kill("SIGTERM"), timeoutMs);
    }
    return new Promise((resolve5, reject) => {
      child.once("error", (error) => {
        debugLog("command.exec.error", {
          processId,
          error: error.message,
          code: error.code
        });
        if (timeout) clearTimeout(timeout);
        this.commandProcesses.delete(processId);
        reject(error);
      });
      child.once("close", (code, signal) => {
        debugLog("command.exec.close", { processId, code, signal, stdoutBytes, stderrBytes });
        if (timeout) clearTimeout(timeout);
        this.commandProcesses.delete(processId);
        resolve5({
          exitCode: code ?? 1,
          stdout: streamOutput ? "" : Buffer.concat(stdout).toString("utf8"),
          stderr: streamOutput ? "" : Buffer.concat(stderr).toString("utf8")
        });
      });
    });
  }
  commandExecWrite(params) {
    const processId = stringOr2(params.processId, "");
    const child = this.commandProcesses.get(processId);
    if (!child) throw new Error(`unknown command process: ${processId}`);
    if (typeof params.deltaBase64 === "string" && params.deltaBase64.length > 0) {
      child.stdin?.write(Buffer.from(params.deltaBase64, "base64"));
    }
    if (params.closeStdin === true) {
      child.stdin?.end();
    }
    return {};
  }
  commandExecTerminate(params) {
    const processId = stringOr2(params.processId, "");
    const child = this.commandProcesses.get(processId);
    if (!child) throw new Error(`unknown command process: ${processId}`);
    child.kill("SIGTERM");
    return {};
  }
  processSpawn(peer, params) {
    const processHandle = stringOr2(params.processHandle, "");
    if (!processHandle) throw new Error("process/spawn requires processHandle");
    if (this.processHandles.has(processHandle))
      throw new Error(`process handle already active: ${processHandle}`);
    const command = commandArray(params.command);
    if (command.length === 0) throw new Error("process/spawn requires command");
    const cwd = stringOr2(params.cwd, process.cwd());
    const isTty = params.tty === true;
    const streamOutput = params.streamStdoutStderr === true || isTty;
    const cap = params.outputBytesCap == null ? 1e6 : numberOr(params.outputBytesCap, 1e6);
    const stdout = [];
    const stderr = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let stdoutCapReached = false;
    let stderrCapReached = false;
    let exited = false;
    debugLog("process.spawn.start", {
      processHandle,
      cwd,
      command,
      streamOutput,
      streamStdin: params.streamStdin === true,
      tty: isTty
    });
    let child;
    const file = fileURLToPath(import.meta.url);
    const candidates = [
      join8(file, "../../../scripts/pty-bridge.py"),
      join8(file, "../../scripts/pty-bridge.py"),
      join8(homedir(), ".local/share/claude-codex/scripts/pty-bridge.py")
    ];
    const ptyBridge = candidates.find((c) => c && existsSync6(c));
    if (isTty && ptyBridge && existsSync6(ptyBridge)) {
      child = spawn3("python3", [ptyBridge, ...command], {
        cwd,
        env: { ...commandEnv(params.env), TERM: "xterm-256color" },
        stdio: ["pipe", "pipe", "inherit"]
      });
      child.__isPtyBridge = true;
    } else {
      child = spawn3(command[0], command.slice(1), {
        cwd,
        env: commandEnv(params.env),
        stdio: "pipe"
      });
    }
    this.processHandles.set(processHandle, child);
    const capture = (target, chunk, currentBytes) => {
      if (currentBytes >= cap) return [currentBytes, true];
      const allowed = Math.min(chunk.byteLength, cap - currentBytes);
      if (allowed > 0) target.push(chunk.subarray(0, allowed));
      return [currentBytes + allowed, allowed < chunk.byteLength];
    };
    if (child.__isPtyBridge) {
      let bufferStr = "";
      child.stdout?.on("data", (chunk) => {
        bufferStr += chunk.toString("utf8");
        const lines = bufferStr.split(/\r?\n/);
        bufferStr = lines.pop() ?? "";
        for (const line of lines) {
          if (!line.trim()) continue;
          try {
            const msg = JSON.parse(line);
            if (msg.stream === "stdout") {
              this.notify(peer, {
                method: "process/outputDelta",
                params: {
                  processHandle,
                  stream: "stdout",
                  deltaBase64: msg.delta,
                  capReached: false
                }
              });
            }
          } catch {
          }
        }
      });
    } else {
      child.stdout?.on("data", (chunk) => {
        const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
        if (streamOutput) {
          this.notify(peer, {
            method: "process/outputDelta",
            params: {
              processHandle,
              stream: "stdout",
              deltaBase64: buffer.toString("base64"),
              capReached: false
            }
          });
        } else {
          ;
          [stdoutBytes, stdoutCapReached] = capture(stdout, buffer, stdoutBytes);
        }
      });
      child.stderr?.on("data", (chunk) => {
        const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
        if (streamOutput) {
          this.notify(peer, {
            method: "process/outputDelta",
            params: {
              processHandle,
              stream: "stderr",
              deltaBase64: buffer.toString("base64"),
              capReached: false
            }
          });
        } else {
          ;
          [stderrBytes, stderrCapReached] = capture(stderr, buffer, stderrBytes);
        }
      });
    }
    const isInteractive = isTty || params.streamStdin === true;
    const defaultTimeout = isInteractive ? 0 : 6e4;
    const timeoutMs = params.timeoutMs == null ? defaultTimeout : numberOr(params.timeoutMs, defaultTimeout);
    const timeout = timeoutMs > 0 ? setTimeout(() => child.kill("SIGTERM"), timeoutMs) : null;
    child.once("error", (error) => {
      debugLog("process.spawn.error", {
        processHandle,
        error: error.message,
        code: error.code
      });
      if (exited) return;
      exited = true;
      if (timeout) clearTimeout(timeout);
      this.processHandles.delete(processHandle);
      setImmediate(
        () => this.notify(peer, {
          method: "process/exited",
          params: {
            processHandle,
            exitCode: 1,
            stdout: streamOutput ? "" : Buffer.concat(stdout).toString("utf8"),
            stdoutCapReached,
            stderr: error.message,
            stderrCapReached: false
          }
        })
      );
    });
    child.once("close", (code, signal) => {
      if (exited) return;
      exited = true;
      debugLog("process.spawn.close", {
        processHandle,
        code,
        signal,
        stdoutBytes,
        stderrBytes,
        stdoutCapReached,
        stderrCapReached
      });
      if (timeout) clearTimeout(timeout);
      this.processHandles.delete(processHandle);
      this.notify(peer, {
        method: "process/exited",
        params: {
          processHandle,
          exitCode: code ?? 1,
          stdout: streamOutput ? "" : Buffer.concat(stdout).toString("utf8"),
          stdoutCapReached,
          stderr: streamOutput ? "" : Buffer.concat(stderr).toString("utf8"),
          stderrCapReached
        }
      });
    });
    return {};
  }
  processWriteStdin(params) {
    const processHandle = stringOr2(params.processHandle, "");
    const child = this.processHandles.get(processHandle);
    if (!child) throw new Error(`unknown process handle: ${processHandle}`);
    if (typeof params.deltaBase64 === "string" && params.deltaBase64.length > 0) {
      if (child.__isPtyBridge) {
        child.stdin?.write(JSON.stringify({ action: "input", data: params.deltaBase64 }) + "\n");
      } else {
        child.stdin?.write(Buffer.from(params.deltaBase64, "base64"));
      }
    }
    if (params.closeStdin === true) child.stdin?.end();
    return {};
  }
  processResizePty(params) {
    const processHandle = stringOr2(params.processHandle, "");
    const child = this.processHandles.get(processHandle);
    if (child && child.__isPtyBridge) {
      const size = params.size || {};
      const cols = numberOr(size.cols, 80);
      const rows = numberOr(size.rows, 24);
      child.stdin?.write(JSON.stringify({ action: "resize", cols, rows }) + "\n");
    }
    return {};
  }
  processKill(params) {
    const processHandle = stringOr2(params.processHandle, "");
    const child = this.processHandles.get(processHandle);
    if (!child) throw new Error(`unknown process handle: ${processHandle}`);
    if (child.__isPtyBridge) {
      child.stdin?.write(JSON.stringify({ action: "kill" }) + "\n");
    }
    child.kill("SIGTERM");
    return {};
  }
  marketplaceAdd(params) {
    const source = stringOr2(params.source, "local");
    const marketplaceName = stringOr2(
      params.refName,
      source.split("/").filter(Boolean).at(-1) ?? "marketplace"
    );
    return {
      marketplaceName,
      installedRoot: `${codexHome()}/marketplaces/${marketplaceName}`,
      alreadyAdded: true
    };
  }
  marketplaceRemove(params) {
    const marketplaceName = stringOr2(params.marketplaceName, "marketplace");
    return { marketplaceName, installedRoot: null };
  }
  marketplaceUpgrade(params) {
    const marketplaceName = typeof params.marketplaceName === "string" ? params.marketplaceName : null;
    return {
      selectedMarketplaces: marketplaceName ? [marketplaceName] : [],
      upgradedRoots: [],
      errors: []
    };
  }
  pluginShareSave(params) {
    const remotePluginId = stringOr2(params.remotePluginId, `local-${newId()}`);
    return {
      remotePluginId,
      shareUrl: `https://localhost.invalid/claude-codex/plugin-share/${encodeURIComponent(remotePluginId)}`
    };
  }
  pluginShareUpdateTargets(params) {
    const shareTargets = Array.isArray(params.shareTargets) ? params.shareTargets : [];
    return {
      principals: shareTargets.map((target) => {
        const rec = asRecord3(target);
        return {
          principalType: stringOr2(rec.principalType, "user"),
          principalId: stringOr2(rec.principalId, ""),
          name: stringOr2(rec.principalId, "unknown")
        };
      }),
      discoverability: params.discoverability === "UNLISTED" ? "UNLISTED" : "PRIVATE"
    };
  }
  configWriteResponse(params) {
    for (const edit of configEdits(params)) {
      const { keyPath, value } = edit;
      if (keyPath === "model" && typeof value === "string" && value.length > 0) {
        this.configModel = normalizeSelectableModelId(value, this.configModel);
      } else if (keyPath === "model_reasoning_effort" && typeof value === "string") {
        this.configReasoningEffort = normalizeCodexReasoningEffort(value) ?? this.configReasoningEffort;
      } else {
        if (value === null || value === void 0) {
          delete this.configOverrides[keyPath];
        } else {
          this.configOverrides[keyPath] = value;
        }
      }
    }
    this.persistConfig();
    const filePath = stringOr2(params.filePath, `${codexHome()}/config.toml`);
    return {
      status: "ok",
      version: `claude-codex-${nowSeconds()}`,
      filePath,
      overriddenMetadata: null
    };
  }
  pluginRead(params) {
    const name2 = stringOr2(params.pluginName, "unknown");
    return {
      plugin: {
        marketplaceName: stringOr2(params.remoteMarketplaceName, "local"),
        marketplacePath: params.marketplacePath ?? null,
        summary: {
          id: name2,
          name: name2,
          shareContext: null,
          source: { type: "remote" },
          installed: false,
          enabled: false,
          installPolicy: "NOT_AVAILABLE",
          authPolicy: "ON_USE",
          availability: "AVAILABLE",
          interface: null,
          keywords: []
        },
        description: null,
        skills: [],
        hooks: [],
        apps: [],
        mcpServers: []
      }
    };
  }
  getConversationSummary(params) {
    const threadId = stringOr2(params.conversationId, "");
    const thread = this.store.getThread(threadId) ?? this.store.listThreads({ limit: 1 }).at(0);
    const now = (/* @__PURE__ */ new Date()).toISOString();
    return {
      summary: {
        conversationId: thread?.id ?? threadId,
        path: "",
        preview: thread?.preview ?? "",
        timestamp: now,
        updatedAt: now,
        modelProvider: thread?.modelProvider ?? "claude-code",
        cwd: thread?.cwd ?? process.cwd(),
        cliVersion: codexCliVersion(),
        source: normalizeSessionSource(thread?.source),
        gitInfo: null
      }
    };
  }
  async gitDiffToRemote(params) {
    const cwd = stringOr2(params.cwd, process.cwd());
    const diff = await gitDiff(cwd);
    let sha = "";
    try {
      const { stdout } = await execFileAsync2("git", ["rev-parse", "HEAD"], { cwd, timeout: 1e4 });
      sha = stdout.trim();
    } catch {
    }
    return { sha, diff };
  }
  async fuzzyFileSearch(params) {
    const query = stringOr2(params.query, "");
    const roots2 = Array.isArray(params.roots) ? params.roots.map(String) : [process.cwd()];
    return { files: await this.fuzzySearchCore(query, roots2) };
  }
  async fuzzySearchCore(rawQuery, roots2) {
    const query = rawQuery.toLowerCase();
    const files = [];
    for (const root of roots2) {
      const paths = await listFiles(root);
      for (const path of paths) {
        const fileName = path.split("/").at(-1) ?? path;
        const haystack = path.toLowerCase();
        if (query && !haystack.includes(query)) continue;
        files.push({
          root,
          path,
          match_type: "file",
          file_name: fileName,
          score: query ? Math.max(1, 100 - haystack.indexOf(query)) : 1,
          indices: null
        });
        if (files.length >= 100) break;
      }
      if (files.length >= 100) break;
    }
    return files;
  }
  fuzzySessionStart(params) {
    const sessionId = stringOr2(params.sessionId, "");
    const roots2 = Array.isArray(params.roots) ? params.roots.map(String) : [process.cwd()];
    if (sessionId) this.fuzzySessions.set(sessionId, { roots: roots2 });
    return {};
  }
  async fuzzySessionUpdate(peer, params) {
    const sessionId = stringOr2(params.sessionId, "");
    const query = stringOr2(params.query, "");
    const session = this.fuzzySessions.get(sessionId);
    const roots2 = session?.roots ?? [process.cwd()];
    const files = await this.fuzzySearchCore(query, roots2);
    if (session && this.fuzzySessions.get(sessionId) === session) {
      this.notify(peer, {
        method: "fuzzyFileSearch/sessionUpdated",
        params: { sessionId, query, files }
      });
    }
    return {};
  }
  fuzzySessionStop(peer, params) {
    const sessionId = stringOr2(params.sessionId, "");
    this.fuzzySessions.delete(sessionId);
    if (sessionId) {
      this.notify(peer, {
        method: "fuzzyFileSearch/sessionCompleted",
        params: { sessionId }
      });
    }
    return {};
  }
  toolUseToItem(event, cwd) {
    const id = newId();
    if (event.toolName === "Bash") {
      return {
        type: "commandExecution",
        id,
        command: String(event.input.command ?? ""),
        cwd: String(event.input.cwd ?? cwd),
        // Use the SDK's tool_use_id as a stable handle. Without a non-null
        // processId the Codex App was treating these as "Background terminal"
        // entries (no attached process) and hiding their output; with a
        // synthetic id they render as inline command items like a normal
        // foreground bash invocation.
        processId: `claude:${event.toolUseId}`,
        source: "agent",
        status: "inProgress",
        commandActions: [],
        aggregatedOutput: null,
        exitCode: null,
        durationMs: null
      };
    }
    if (["Edit", "Write", "MultiEdit"].includes(event.toolName)) {
      return {
        type: "fileChange",
        id,
        changes: fileChangeFromTool(event.toolName, event.input),
        status: "inProgress"
      };
    }
    if (event.toolName === "WebSearch") {
      const q = String(event.input.query ?? "");
      return {
        type: "webSearch",
        id,
        query: q,
        action: { type: "search", query: q || null, queries: null }
      };
    }
    let displayTool = event.toolName;
    if (event.toolName === "Read") {
      const raw = String(event.input.file_path || event.input.path || "");
      if (raw) {
        let displayPath = raw;
        try {
          if (cwd && raw.startsWith(cwd)) {
            displayPath = raw.slice(cwd.length).replace(/^\/+/, "");
          } else {
            const parts = raw.split(/\//).filter(Boolean);
            displayPath = parts.length > 2 ? parts.slice(-2).join("/") : raw;
          }
        } catch {
        }
        displayTool = `Read ${displayPath || raw}`;
      }
    } else if (event.toolName === "Grep") {
      const pat = String(event.input.pattern ?? "");
      const rawPath = event.input.path ? String(event.input.path) : "";
      let pathStr = "";
      if (rawPath) {
        const parts = rawPath.split(/\//).filter(Boolean);
        pathStr = ` (${parts.length > 2 ? parts.slice(-2).join("/") : rawPath})`;
      }
      if (pat) displayTool = `Grep ${pat}${pathStr}`;
    } else if (event.toolName === "Glob") {
      const pat = String(event.input.pattern ?? "");
      if (pat) displayTool = `Glob ${pat}`;
    }
    return {
      type: "mcpToolCall",
      id,
      server: "claude-code",
      tool: displayTool,
      status: "inProgress",
      arguments: event.input,
      result: null,
      error: null,
      durationMs: null
    };
  }
  threadEnvelope(thread, turns = []) {
    const activePermissionProfileId = threadPermissionProfileId(
      thread.permissionProfileId,
      thread.approvalPolicy,
      thread.sandboxMode
    );
    return {
      thread: this.toThread(thread, turns),
      model: thread.model,
      modelProvider: thread.modelProvider,
      serviceTier: null,
      cwd: thread.cwd,
      runtimeWorkspaceRoots: [thread.cwd],
      instructionSources: [],
      approvalPolicy: thread.approvalPolicy ?? "never",
      approvalsReviewer: "user",
      sandbox: sandboxEnvelope(thread.sandboxMode, thread.cwd),
      permissionProfile: null,
      activePermissionProfile: activePermissionProfileId ? { id: activePermissionProfileId, extends: null } : null,
      // Some newer clients read the compact id while older clients use the
      // structured activePermissionProfile field. Return both; unknown extra
      // fields are ignored by the legacy app-server schema.
      permissions: activePermissionProfileId,
      reasoningEffort: thread.reasoningEffort,
      multiAgentMode: "explicitRequestOnly"
    };
  }
  toThread(thread, turns = []) {
    const agentNickname = nullIfEmpty(thread.agentNickname);
    const agentRole = nullIfEmpty(thread.agentRole);
    const parentThreadId = thread.threadSource === "subagent" ? thread.forkedFromId : null;
    const source = parentThreadId ? {
      subAgent: {
        thread_spawn: {
          parent_thread_id: parentThreadId,
          depth: this.subagentDepth(thread),
          agent_path: null,
          agent_nickname: agentNickname,
          agent_role: agentRole
        }
      }
    } : normalizeSessionSource(thread.source);
    const profileId = thread.sandboxMode === "danger-full-access" ? ":danger-full-access" : thread.sandboxMode === "read-only" ? ":read-only" : ":workspace";
    const section = thread.sectionId == null ? null : this.store.getSection(thread.sectionId);
    const isPinned = thread.sectionId === PINNED_SECTION_ID || thread.isPinned === true;
    return {
      id: thread.id,
      isPinned,
      section,
      sectionEnteredAt: thread.sectionEnteredAt ?? null,
      sessionId: thread.sessionId,
      forkedFromId: thread.forkedFromId,
      parentThreadId,
      preview: thread.preview,
      projectId: this.projects.projectId(thread),
      ephemeral: thread.ephemeral,
      modelProvider: thread.modelProvider,
      createdAt: thread.createdAt,
      updatedAt: thread.updatedAt,
      recencyAt: thread.updatedAt,
      status: thread.status,
      path: null,
      cwd: thread.cwd,
      cliVersion: codexCliVersion(),
      canAcceptDirectInput: true,
      model: thread.model,
      reasoningEffort: thread.reasoningEffort,
      collaborationMode: { mode: thread.collaborationMode ?? "default", settings: { model: thread.model, reasoning_effort: thread.reasoningEffort, developer_instructions: thread.developerInstructions } },
      activePermissionProfile: profileId,
      // Defense-in-depth: even if older rows hold an invalid `source` or
      // `threadSource` (legacy `app_server`, empty string from a buggy write
      // path), coerce on the way out so the App's strict deserializer never
      // sees a value outside the wire enum.
      source,
      threadSource: normalizeThreadSource(thread.threadSource),
      agentNickname,
      agentRole,
      gitInfo: null,
      name: thread.name,
      turns: turns.map((turn) => this.toTurn(turn))
    };
  }
  subagentDepth(thread) {
    let depth = thread.threadSource === "subagent" ? 1 : 0;
    let ancestorId = thread.forkedFromId;
    const seen = /* @__PURE__ */ new Set([thread.id]);
    while (ancestorId && !seen.has(ancestorId)) {
      seen.add(ancestorId);
      const ancestor = this.store.getThread(ancestorId);
      if (!ancestor || ancestor.threadSource !== "subagent") break;
      depth += 1;
      ancestorId = ancestor.forkedFromId;
    }
    return depth;
  }
  // Full turn payload for history reads (thread/read, turns/list) — carries the
  // loaded items. The Codex v2 `Turn` schema has no api/cost metadata fields, so
  // the adapter's internal metrics are not serialized onto the wire.
  toTurn(turn) {
    return {
      id: turn.id,
      items: turn.items,
      itemsView: "full",
      status: turn.status,
      error: turn.error,
      startedAt: turn.startedAt,
      completedAt: turn.completedAt,
      durationMs: turn.durationMs
    };
  }
  toTurnView(turn, itemsView) {
    if (itemsView === "full") return this.toTurn(turn);
    if (itemsView === "notLoaded") return this.toLifecycleTurn(turn);
    const firstUser = turn.items.find((item) => item.type === "userMessage");
    const lastAgent = turn.items.findLast((item) => item.type === "agentMessage");
    const items = [];
    if (firstUser) items.push(firstUser);
    if (lastAgent && lastAgent.id !== firstUser?.id) items.push(lastAgent);
    return {
      id: turn.id,
      items,
      itemsView: "summary",
      status: turn.status,
      error: turn.error,
      startedAt: turn.startedAt,
      completedAt: turn.completedAt,
      durationMs: turn.durationMs
    };
  }
  // Lightweight payload for turn/start, turn/started, and terminal paths that
  // intentionally do not carry an assistant summary. The item stream drives
  // the timeline; completed subagent turns use toCompletedTurn below.
  toLifecycleTurn(turn, items = []) {
    return {
      id: turn.id,
      items,
      itemsView: "notLoaded",
      status: turn.status,
      error: turn.error,
      startedAt: turn.startedAt,
      completedAt: turn.completedAt,
      durationMs: turn.durationMs
    };
  }
  // Codex includes the final assistant message in successful turn/completed
  // notifications. Subagent pages depend on that summary because they can be
  // opened with a metadata-only thread/read and may not replay prior deltas.
  toCompletedTurn(turn) {
    const lastAgent = turn.status === "completed" && turn.error == null ? turn.items.findLast((item) => item.type === "agentMessage" && item.text.trim().length > 0) : void 0;
    return {
      id: turn.id,
      items: lastAgent ? [lastAgent] : [],
      itemsView: lastAgent ? "summary" : "notLoaded",
      status: turn.status,
      error: turn.error,
      startedAt: turn.startedAt,
      completedAt: turn.completedAt,
      durationMs: turn.durationMs
    };
  }
  sendResponse(peer, id, result, error) {
    const response = { jsonrpc: "2.0", id };
    if (error) response.error = error;
    else response.result = result ?? null;
    peer.send(response);
  }
  notify(peer, notification) {
    if (this.stopped) return;
    const target = this.peerForParams(peer, notification.params);
    debugLog("rpc.notify", {
      peerId: target.id,
      originalPeerId: target.id === peer.id ? null : peer.id,
      method: notification.method,
      params: summarizeRpcParams(notification.method, notification.params)
    });
    target.send({ jsonrpc: "2.0", method: notification.method, params: notification.params });
  }
  notifyThread(threadId, notification) {
    const peer = this.activePeerByThread.get(threadId);
    if (peer) this.notify(peer, notification);
  }
  setThreadStatus(peer, threadId, status) {
    this.store.updateThreadStatus(threadId, status);
    const target = peer ?? this.activePeerByThread.get(threadId);
    if (target)
      this.notify(target, { method: "thread/status/changed", params: { threadId, status } });
  }
  sendServerRequest(peer, method, id, params) {
    const target = this.peerForParams(peer, params);
    const key = `${target.id}:${id}`;
    return new Promise((resolve5, reject) => {
      debugLog("rpc.serverRequest", {
        peerId: target.id,
        originalPeerId: target.id === peer.id ? null : peer.id,
        id,
        method,
        params: summarizeRpcParams(method, params)
      });
      this.pendingServerRequests.set(key, { resolve: resolve5, reject });
      target.send({ jsonrpc: "2.0", id, method, params });
    });
  }
  resolveServerRequest(response) {
    for (const [key, pending] of this.pendingServerRequests.entries()) {
      if (key.endsWith(`:${String(response.id)}`)) {
        this.pendingServerRequests.delete(key);
        if (response.error) pending.reject(new Error(response.error.message));
        else pending.resolve(response.result);
        return;
      }
    }
  }
  completeActiveTurns(status, error) {
    for (const [threadId, turnId] of this.activeTurnByThread.entries()) {
      const turn = this.store.getTurn(turnId);
      if (turn?.status === "inProgress") {
        this.store.completeTurn(turnId, status, error);
      }
      this.store.updateThreadStatus(threadId, { type: "idle" });
    }
    this.activeTurnByThread.clear();
  }
  finalizeActiveSubagentsForShutdown(message) {
    for (const [turnId, state] of this.subagentStateByTurn.entries()) {
      const turn = this.store.getTurn(turnId);
      if (!turn || turn.status !== "inProgress") {
        this.subagentStateByTurn.delete(turnId);
        continue;
      }
      const peer = this.activePeerByThread.get(state.thread.id) ?? {
        id: "shutdown",
        send: () => {
        },
        close: () => {
        }
      };
      this.finalizeOrphanedSubagents(
        peer,
        state.thread,
        turn,
        state.contexts,
        state.active,
        message
      );
      this.subagentStateByTurn.delete(turnId);
    }
  }
  clearActiveTurn(threadId) {
    const existed = this.activeTurnByThread.delete(threadId);
    if (existed && this.activeTurnByThread.size === 0) this.idleCheckHandler?.();
  }
  peerForParams(peer, params) {
    const threadId = stringOr2(asRecord3(params).threadId, "");
    if (!threadId) return peer;
    const direct = this.activePeerByThread.get(threadId);
    if (direct) return direct;
    const seen = /* @__PURE__ */ new Set();
    let current = this.store.getThread(threadId);
    while (current?.forkedFromId && !seen.has(current.forkedFromId)) {
      seen.add(current.forkedFromId);
      const ancestorPeer = this.activePeerByThread.get(current.forkedFromId);
      if (ancestorPeer) {
        this.activePeerByThread.set(threadId, ancestorPeer);
        return ancestorPeer;
      }
      current = this.store.getThread(current.forkedFromId);
    }
    return peer;
  }
  bindPeerToDescendants(peer, rootThreadId) {
    const queue = [rootThreadId];
    const seen = /* @__PURE__ */ new Set();
    while (queue.length > 0) {
      const parentId = queue.shift();
      if (!parentId || seen.has(parentId)) continue;
      seen.add(parentId);
      for (const child of this.store.listThreads({
        includeEphemeral: true,
        parentThreadId: parentId,
        sortKey: "created_at",
        sortDirection: "asc"
      })) {
        this.activePeerByThread.set(child.id, peer);
        queue.push(child.id);
      }
    }
  }
  loadPersistedConfig() {
    try {
      const parsed = JSON.parse(readFileSync6(this.configPath, "utf8"));
      let shouldRepair = false;
      if (typeof parsed.model === "string" && parsed.model.length > 0) {
        const normalized = normalizeSelectableModelId(parsed.model, this.configModel);
        shouldRepair = normalized !== parsed.model;
        this.configModel = normalized;
      }
      if (typeof parsed.model_reasoning_effort === "string") {
        this.configReasoningEffort = normalizeCodexReasoningEffort(parsed.model_reasoning_effort) ?? this.configReasoningEffort;
      }
      if (parsed.overrides && typeof parsed.overrides === "object" && !Array.isArray(parsed.overrides)) {
        this.configOverrides = parsed.overrides;
      }
      if (shouldRepair) this.persistConfig();
    } catch {
    }
  }
  persistConfig() {
    try {
      ensureParent(this.configPath);
      writeFileSync3(
        this.configPath,
        JSON.stringify(
          {
            model: this.configModel,
            model_reasoning_effort: this.configReasoningEffort,
            overrides: this.configOverrides
          },
          null,
          2
        ) + "\n",
        { mode: 384 }
      );
    } catch {
    }
  }
};

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/native-runtime.mts
import { copyFileSync, existsSync as existsSync7, mkdirSync as mkdirSync4, readFileSync as readFileSync7, readdirSync as readdirSync3, realpathSync as realpathSync2 } from "node:fs";
import { join as join10 } from "node:path";

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/workflow-subagents.mts
import { constants } from "node:fs";
import { lstat, open, realpath } from "node:fs/promises";
import { basename, isAbsolute as isAbsolute2, join as join9, normalize as normalize2, relative, resolve as resolve4, sep } from "node:path";
var MAX_WORKFLOW_JOURNAL_BYTES = 8 * 1024 * 1024;
var MAX_WORKFLOW_AGENT_TRANSCRIPT_BYTES = 4 * 1024 * 1024;
var WORKFLOW_DRAIN_SETTLE_MS = 500;
function defaultWorkflowTranscriptRoots(env = process.env, cwd = process.cwd()) {
  const configuredRoot = stringValue(env.CLAUDE_CONFIG_DIR);
  const home = stringValue(env.HOME) || homedir();
  return [resolve4(cwd, configuredRoot || join9(home, ".claude"))];
}
function parseWorkflowLaunchInfo(value, transcriptRoots = defaultWorkflowTranscriptRoots()) {
  const launch = recordOrNull(value);
  if (!launch) return null;
  if (launch.taskType == null) launch.taskType = launch.task_type;
  if (launch.taskType == null) launch.taskType = "local_workflow";
  if (launch.taskType === "workflow") launch.taskType = "local_workflow";
  if (launch.taskId == null) launch.taskId = launch.task_id;
  if (launch.runId == null) launch.runId = launch.run_id;
  if (launch.transcriptDir == null) launch.transcriptDir = launch.transcript_dir;
  if (launch.workflowName == null) launch.workflowName = launch.workflow_name;
  if (String(launch.status ?? "") !== "async_launched") return null;
  if (String(launch.taskType ?? "") !== "local_workflow") return null;
  const taskId = stringValue(launch.taskId);
  const runId = stringValue(launch.runId);
  const transcriptDir = stringValue(launch.transcriptDir);
  const normalizedTranscriptDir = normalize2(transcriptDir);
  const transcriptRoot = trustedTranscriptRoot(normalizedTranscriptDir, transcriptRoots);
  if (!taskId || !runId || !transcriptDir || !isWorkflowTranscriptDir(normalizedTranscriptDir, runId) || !transcriptRoot) {
    return null;
  }
  return {
    taskId,
    runId,
    transcriptDir: normalizedTranscriptDir,
    transcriptRoot,
    workflowName: stringValue(launch.workflowName ?? launch.workflow_name),
    summary: stringValue(launch.summary)
  };
}
var WorkflowJournalMonitor = class {
  launch;
  pollIntervalMs;
  onStarted;
  onResult;
  onError;
  startedAgentIds = /* @__PURE__ */ new Set();
  completedAgentIds = /* @__PURE__ */ new Set();
  timer = null;
  pollPromise = null;
  verifiedTranscriptDir = null;
  failure = null;
  generation = 0;
  stopped = false;
  constructor(options) {
    this.launch = options.launch;
    this.pollIntervalMs = Math.max(10, options.pollIntervalMs ?? 100);
    this.onStarted = options.onStarted;
    this.onResult = options.onResult;
    this.onError = options.onError;
  }
  start() {
    if (this.stopped || this.timer) return;
    this.timer = setInterval(() => {
      void this.flush();
    }, this.pollIntervalMs);
    this.timer.unref();
    void this.flush();
  }
  async flush() {
    if (this.stopped || this.failure) return;
    if (this.pollPromise) return this.pollPromise;
    const generation = this.generation;
    this.pollPromise = this.pollOnce(generation).catch(async (error) => {
      this.fail(error);
      const failure = this.failure;
      if (failure && this.onError) {
        try {
          await this.onError(failure);
        } catch {
        }
      }
    }).finally(() => {
      this.pollPromise = null;
    });
    return this.pollPromise;
  }
  async drain(timeoutMs = 500) {
    const deadline = Date.now() + Math.max(0, timeoutMs);
    let stableSince = Date.now();
    let lastStarted = this.startedAgentIds.size;
    let lastCompleted = this.completedAgentIds.size;
    while (Date.now() <= deadline) {
      const remaining = Math.max(0, deadline - Date.now());
      if (!await settlesWithin(this.flush(), remaining)) return;
      if (this.stopped || this.failure) return;
      const now = Date.now();
      const started = this.startedAgentIds.size;
      const completed = this.completedAgentIds.size;
      if (started !== lastStarted || completed !== lastCompleted) {
        lastStarted = started;
        lastCompleted = completed;
        stableSince = now;
      }
      if (started > 0 && completed >= started && now - stableSince >= WORKFLOW_DRAIN_SETTLE_MS) {
        return;
      }
      if (now >= deadline) return;
      await delay(Math.min(this.pollIntervalMs, Math.max(1, deadline - now)));
    }
  }
  async stop(timeoutMs = 500) {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    this.stopped = true;
    this.generation += 1;
    if (this.pollPromise) {
      await settlesWithin(this.pollPromise, Math.max(0, timeoutMs));
    }
  }
  get startedCount() {
    return this.startedAgentIds.size;
  }
  activeAgentIds() {
    return [...this.startedAgentIds].filter((agentId) => !this.completedAgentIds.has(agentId));
  }
  async pollOnce(generation) {
    if (!this.isCurrent(generation)) return;
    const transcriptDir = await this.resolveTranscriptDir();
    if (!transcriptDir || !this.isCurrent(generation)) return;
    const entries = await readJournalEntries(join9(transcriptDir, "journal.jsonl"), transcriptDir);
    if (!this.isCurrent(generation)) return;
    for (const entry of entries) {
      if (!this.isCurrent(generation)) return;
      const started = await this.ensureStarted(transcriptDir, entry.agentId, generation);
      if (!this.isCurrent(generation)) return;
      if (!started) continue;
      if (entry.type !== "result" || this.completedAgentIds.has(entry.agentId)) continue;
      const result = await workflowAgentResult(transcriptDir, entry.agentId, entry.result);
      if (!result || !this.isCurrent(generation)) continue;
      await this.onResult(result);
      if (!this.isCurrent(generation)) return;
      this.completedAgentIds.add(entry.agentId);
    }
  }
  async ensureStarted(transcriptDir, agentId, generation) {
    if (this.startedAgentIds.has(agentId)) return true;
    const prompt = await readAgentPrompt(transcriptDir, agentId);
    if (!prompt || !this.isCurrent(generation)) return false;
    await this.onStarted({
      agentId,
      prompt,
      description: workflowAgentDescription(prompt, agentId)
    });
    if (!this.isCurrent(generation)) return false;
    this.startedAgentIds.add(agentId);
    return true;
  }
  async resolveTranscriptDir() {
    if (this.verifiedTranscriptDir) return this.verifiedTranscriptDir;
    let transcriptRoot;
    let transcriptDir;
    try {
      ;
      [transcriptRoot, transcriptDir] = await Promise.all([
        realpath(this.launch.transcriptRoot),
        realpath(this.launch.transcriptDir)
      ]);
    } catch (error) {
      if (isMissingFile(error)) return null;
      throw error;
    }
    if (!isPathInside(transcriptRoot, transcriptDir) || !isWorkflowTranscriptDir(transcriptDir, this.launch.runId)) {
      throw new Error("Workflow transcript directory is outside the trusted Claude config root");
    }
    this.verifiedTranscriptDir = transcriptDir;
    return transcriptDir;
  }
  fail(error) {
    this.failure = error instanceof Error ? error : new Error(String(error));
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    this.stopped = true;
    this.generation += 1;
  }
  isCurrent(generation) {
    return !this.stopped && !this.failure && this.generation === generation;
  }
};
function isWorkflowTranscriptDir(path, runId) {
  if (!isAbsolute2(path) || basename(normalize2(path)) !== runId) return false;
  const parts = normalize2(path).split(sep).filter(Boolean);
  const workflowIndex = parts.lastIndexOf("workflows");
  return workflowIndex > 0 && workflowIndex === parts.length - 2 && parts[workflowIndex - 1] === "subagents";
}
function trustedTranscriptRoot(path, roots2) {
  if (!isAbsolute2(path)) return null;
  const normalizedRoots = roots2.map((root) => stringValue(root)).filter(Boolean).map((root) => resolve4(root)).sort((left, right) => right.length - left.length);
  return normalizedRoots.find((root) => isPathInside(root, path)) ?? null;
}
function isPathInside(root, path) {
  const child = relative(root, path);
  return child !== "" && child !== ".." && !child.startsWith(`..${sep}`) && !isAbsolute2(child);
}
async function readJournalEntries(path, transcriptDir) {
  const text = await readRegularTextFile(path, transcriptDir, MAX_WORKFLOW_JOURNAL_BYTES);
  if (text === null) return [];
  const entries = [];
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const entry = recordOrNull(parsed);
    if (!entry) continue;
    const type = String(entry.type ?? "");
    const agentId = stringValue(entry.agentId);
    if (type !== "started" && type !== "result" || !isWorkflowAgentId(agentId)) continue;
    entries.push({
      type,
      agentId,
      ...type === "result" ? { result: entry.result } : {}
    });
  }
  return entries;
}
async function readAgentPrompt(transcriptDir, agentId) {
  const text = await readRegularTextFile(
    join9(transcriptDir, `agent-${agentId}.jsonl`),
    transcriptDir,
    MAX_WORKFLOW_AGENT_TRANSCRIPT_BYTES
  );
  if (text === null) return null;
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const record = recordOrNull(parsed);
    const message = recordOrNull(record?.message);
    if (message?.role !== "user") continue;
    const prompt = messageText(message.content);
    if (prompt) return prompt;
  }
  return null;
}
async function readRegularTextFile(path, transcriptDir, maxBytes) {
  let canonicalBefore;
  try {
    canonicalBefore = await realpath(path);
  } catch (error) {
    if (isMissingFile(error)) return null;
    throw error;
  }
  if (!isPathInside(transcriptDir, canonicalBefore)) {
    throw new Error("Workflow transcript file is outside the verified transcript directory");
  }
  let handle;
  try {
    handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  } catch (error) {
    if (isMissingFile(error)) return null;
    throw error;
  }
  try {
    const openedEntry = await handle.stat();
    if (!openedEntry.isFile()) {
      throw new Error("Workflow transcript entry is not a regular file");
    }
    if (openedEntry.size > maxBytes) {
      throw new Error("Workflow transcript entry exceeds the configured size limit");
    }
    const [canonicalAfter, currentEntry] = await Promise.all([realpath(path), lstat(path)]);
    if (canonicalAfter !== canonicalBefore || !isPathInside(transcriptDir, canonicalAfter) || !currentEntry.isFile() || currentEntry.isSymbolicLink() || currentEntry.dev !== openedEntry.dev || currentEntry.ino !== openedEntry.ino) {
      throw new Error("Workflow transcript entry changed during validation");
    }
    return await handle.readFile("utf8");
  } catch (error) {
    if (isMissingFile(error)) return null;
    throw error;
  } finally {
    await handle.close();
  }
}
async function workflowAgentResult(transcriptDir, agentId, result) {
  const resultRecord = recordOrNull(result);
  const status = String(resultRecord?.status ?? "").trim().toLowerCase();
  const isError = Boolean(resultRecord?.error) || status === "error" || status === "errored" || status === "failed" || status === "failure";
  const body = resultText(result) || await readAgentResult(transcriptDir, agentId);
  if (!body) return null;
  return {
    agentId,
    content: `${body}
agentId: ${agentId}`,
    isError
  };
}
async function readAgentResult(transcriptDir, agentId) {
  const text = await readRegularTextFile(
    join9(transcriptDir, `agent-${agentId}.jsonl`),
    transcriptDir,
    MAX_WORKFLOW_AGENT_TRANSCRIPT_BYTES
  );
  if (text === null) return null;
  let finalText = "";
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const record = recordOrNull(parsed);
    const message = recordOrNull(record?.message);
    if (message?.role !== "assistant") continue;
    const content = messageText(message.content);
    if (content) finalText = content;
  }
  return finalText || null;
}
function workflowAgentDescription(prompt, agentId) {
  const firstLine = prompt.split("\n").map((line) => line.trim()).find(Boolean);
  return (firstLine || `Workflow agent ${agentId}`).slice(0, 120);
}
function resultText(value) {
  if (typeof value === "string") return value.trim();
  if (value == null) return "";
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
function messageText(value) {
  if (typeof value === "string") return value.trim();
  if (!Array.isArray(value)) return "";
  return value.map((block) => {
    const record = recordOrNull(block);
    return record?.type === "text" ? stringValue(record.text) : "";
  }).filter(Boolean).join("\n").trim();
}
function isWorkflowAgentId(value) {
  return /^[0-9a-f]{8,32}$/i.test(value);
}
function recordOrNull(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}
function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}
function isMissingFile(error) {
  return error instanceof Error && "code" in error && (error.code === "ENOENT" || error.code === "ENOTDIR");
}
function delay(ms) {
  return new Promise((resolve5) => setTimeout(resolve5, ms));
}
async function settlesWithin(promise, timeoutMs) {
  if (timeoutMs <= 0) return false;
  let timer = null;
  try {
    return await Promise.race([
      promise.then(() => true),
      new Promise((resolveTimeout) => {
        timer = setTimeout(() => resolveTimeout(false), timeoutMs);
      })
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

// outputs/TASK-0089/claude-protocol-reference-inputs-v1/src/native-runtime.mts
if (!process.env.ANTHROPIC_BETAS) {
  process.env.ANTHROPIC_BETAS = "extended-cache-ttl-2025-04-11";
} else if (!process.env.ANTHROPIC_BETAS.includes("extended-cache-ttl-2025-04-11")) {
  process.env.ANTHROPIC_BETAS = `${process.env.ANTHROPIC_BETAS},extended-cache-ttl-2025-04-11`;
}
var paidConfigurationKeys = [
  "ANTHROPIC_API_KEY",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_BASE_URL",
  "CLAUDE_CODE_USE_BEDROCK",
  "CLAUDE_CODE_USE_VERTEX",
  "CLAUDE_CODE_USE_FOUNDRY"
];
function assertSubscriptionBilling(cwd) {
  if (process.env.CLAUDE_CODEX_SUBSCRIPTION_ONLY !== "1") return;
  if (!process.env.CLAUDE_CONFIG_DIR || paidConfigurationKeys.some((key) => process.env[key]))
    throw new Error("Claude subscription mode conflicts with API or cloud gateway environment settings");
  for (const path of [
    join10(process.env.CLAUDE_CONFIG_DIR, "settings.json"),
    join10(process.env.CLAUDE_CONFIG_DIR, "settings.local.json"),
    join10(cwd, ".claude", "settings.json"),
    join10(cwd, ".claude", "settings.local.json")
  ]) {
    if (!existsSync7(path)) continue;
    const settings = JSON.parse(readFileSync7(path, "utf8"));
    if (!settings || typeof settings !== "object" || Array.isArray(settings))
      throw new Error("Claude settings must contain an object");
    const env = settings.env && typeof settings.env === "object" && !Array.isArray(settings.env) ? settings.env : {};
    if (paidConfigurationKeys.some((key) => Object.hasOwn(env, key)) || Object.hasOwn(settings, "apiKeyHelper") || settings.forceLoginMethod === "console")
      throw new Error("Claude subscription mode conflicts with API or cloud gateway settings");
  }
}
var WORKFLOW_TERMINAL_FLUSH_TIMEOUT_MS = 500;
var WORKFLOW_JOURNAL_SETTLE_TIMEOUT_MS = 3e3;
var NativeClaudeRuntime = class {
  sdk = null;
  turns = /* @__PURE__ */ new Map();
  permissions = /* @__PURE__ */ new Map();
  async supportedModels() {
    assertSubscriptionBilling(process.cwd());
    const sdk = await this.loadSdk();
    const abort = new AbortController();
    const query = sdk.query({
      // An empty input stream initializes the CLI without submitting an inference turn.
      prompt: (async function* () {
      })(),
      options: {
        cwd: process.cwd(),
        settingSources: ["user", "project", "local"],
        ...process.env.CLAUDE_CODEX_CLI ? { pathToClaudeCodeExecutable: process.env.CLAUDE_CODEX_CLI } : {},
        persistSession: false,
        tools: [],
        mcpServers: {},
        abortController: abort
      }
    });
    const timer = setTimeout(() => abort.abort(), 3e4);
    try {
      return await query.supportedModels();
    } finally {
      clearTimeout(timer);
      query.close();
    }
  }
  // Reads the plan windows behind `/usage` without an inference turn or a transcript scan. The SDK
  // marks this request experimental; the pinned SDK version keeps its shape fixed for this build.
  async planUsage() {
    assertSubscriptionBilling(process.cwd());
    const sdk = await this.loadSdk();
    const abort = new AbortController();
    const query = sdk.query({
      prompt: (async function* () {
      })(),
      options: {
        cwd: process.cwd(),
        settingSources: ["user", "project", "local"],
        ...process.env.CLAUDE_CODEX_CLI ? { pathToClaudeCodeExecutable: process.env.CLAUDE_CODEX_CLI } : {},
        persistSession: false,
        tools: [],
        mcpServers: {},
        abortController: abort
      }
    });
    const timer = setTimeout(() => abort.abort(), 15e3);
    try {
      const usage = await query.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET({ skipBehaviors: true });
      return usage.rate_limits_available ? usage.rate_limits : null;
    } finally {
      clearTimeout(timer);
      query.close();
    }
  }
  async runTurn(context, handlers) {
    const sdk = await this.loadSdk();
    const abort = new AbortController();
    return new Promise((resolve5, reject) => {
      const promptIterable = this.buildPromptIterable(context);
      const options = this.buildOptions(sdk, context, abort);
      const query = sdk.query({ prompt: promptIterable, options });
      const pending = {
        context,
        handlers,
        query,
        abort,
        resolved: false,
        resolve: resolve5,
        reject,
        assistantMessageId: null,
        streamedBlocks: /* @__PURE__ */ new Map(),
        activeSubagents: /* @__PURE__ */ new Set(),
        completedWorkflowTasks: /* @__PURE__ */ new Set(),
        workflowToolUseIds: /* @__PURE__ */ new Set(),
        workflowLaunches: /* @__PURE__ */ new Map(),
        workflowTranscriptRoots: defaultWorkflowTranscriptRoots(process.env, context.cwd),
        skippedWorkflowTaskIds: /* @__PURE__ */ new Set(),
        workflowTasks: /* @__PURE__ */ new Map(),
        toolStartSeen: /* @__PURE__ */ new Set(),
        structuredBuffer: "",
        pendingUserMessage: null,
        deferredResult: null,
        workflowFailure: null
      };
      this.turns.set(context.turnId, pending);
      void this.consume(pending).catch((err) => {
        if (!pending.resolved) {
          pending.resolved = true;
          this.turns.delete(context.turnId);
          reject(err instanceof Error ? err : new Error(String(err)));
        }
      });
    });
  }
  async steer(threadId, prompt) {
    for (const pending of this.turns.values()) {
      if (pending.context.threadId !== threadId) continue;
      const q = pending.query;
      if (typeof q.streamInput === "function") {
        q.streamInput(
          (async function* () {
            yield {
              type: "user",
              message: { role: "user", content: workflowRuntimePrompt(prompt) },
              parent_tool_use_id: null,
              origin: { kind: "human" }
            };
          })()
        );
      }
      return;
    }
  }
  async interrupt(threadId) {
    for (const pending of this.turns.values()) {
      if (pending.context.threadId === threadId) {
        await this.stopWorkflowTasks(pending);
        pending.abort.abort();
        await pending.query.interrupt().catch(() => {
        });
      }
    }
  }
  async stop() {
    for (const pending of this.turns.values()) {
      await this.stopWorkflowTasks(pending);
      pending.abort.abort();
    }
    this.turns.clear();
    this.permissions.clear();
  }
  // ── private ──
  async loadSdk() {
    if (this.sdk) return this.sdk;
    this.sdk = await import("@anthropic-ai/claude-agent-sdk");
    return this.sdk;
  }
  buildPromptIterable(context) {
    const text = workflowRuntimePrompt(context.prompt);
    const images = context.imageInputs;
    return (async function* () {
      if (!images || images.length === 0) {
        yield {
          type: "user",
          message: { role: "user", content: text },
          parent_tool_use_id: null,
          origin: { kind: "human" }
        };
        return;
      }
      const content = [];
      if (text) content.push({ type: "text", text });
      for (const img of images) {
        if (img.kind === "base64") {
          content.push({
            type: "image",
            source: { type: "base64", media_type: img.mediaType, data: img.data }
          });
        } else {
          content.push({
            type: "image",
            source: { type: "url", url: img.data }
          });
        }
      }
      yield {
        type: "user",
        message: { role: "user", content },
        parent_tool_use_id: null,
        origin: { kind: "human" }
      };
    })();
  }
  buildOptions(sdk, context, abort) {
    assertSubscriptionBilling(context.cwd);
    const opts = {
      settingSources: ["user", "project", "local"],
      abortController: abort,
      includePartialMessages: true,
      includeHookEvents: true,
      cwd: context.cwd
    };
    if (context.model) opts.model = context.model;
    if (context.effort) opts.effort = context.effort;
    const resume = sdkResumeSessionId(context.claudeSessionId, context.cwd);
    if (resume) opts.resume = resume;
    if (resume && context.forkSession) opts.forkSession = true;
    if (!process.env.ANTHROPIC_BETAS) {
      process.env.ANTHROPIC_BETAS = "extended-cache-ttl-2025-04-11";
    }
    if (context.addDirs && context.addDirs.length > 0) opts.additionalDirectories = context.addDirs;
    if (context.allowedTools && context.allowedTools.length > 0)
      opts.allowedTools = context.allowedTools;
    if (context.mcpServers && typeof context.mcpServers === "object")
      opts.mcpServers = context.mcpServers;
    if (context.outputFormat) opts.outputFormat = context.outputFormat;
    const permissionModeOverride = configuredPermissionMode();
    const mode = derivePermissionMode(context.approvalPolicy, context.sandboxMode, context.planMode);
    opts.permissionMode = mode;
    if (mode === "bypassPermissions") opts.allowDangerouslySkipPermissions = true;
    if (parseWorkflowCommand(context.prompt)?.type === "run") {
      opts.settings = {
        ...opts.settings ?? {},
        enableWorkflows: true,
        workflowKeywordTriggerEnabled: true
      };
    }
    if (mode !== "plan" && mode !== "bypassPermissions") {
      const appFullAccess = permissionModeOverride === null && (context.approvalPolicy === "never" || context.sandboxMode === "danger-full-access");
      const autoAllow = appFullAccess || mode === "dontAsk";
      opts.canUseTool = this.makeCanUseTool(context, autoAllow);
    }
    const managedInstructions = process.env.CLAUDE_CODEX_INSTRUCTIONS_FILE && existsSync7(process.env.CLAUDE_CODEX_INSTRUCTIONS_FILE) ? readFileSync7(process.env.CLAUDE_CODEX_INSTRUCTIONS_FILE, "utf8") : "";
    const includeLocal = /^@AGENTS\.md\s*$/m.test(managedInstructions);
    const localInstructions = includeLocal && existsSync7(join10(context.cwd, "AGENTS.md")) ? readFileSync7(join10(context.cwd, "AGENTS.md"), "utf8") : "";
    const appendedInstructions = [
      managedInstructions.replace(/^@AGENTS\.md\s*$/gm, "").trim(),
      localInstructions.trim(),
      context.systemPromptAddendum?.trim() ?? ""
    ].filter(Boolean).join("\n\n");
    opts.systemPrompt = {
      type: "preset",
      preset: "claude_code",
      snapshot: false,
      ...appendedInstructions ? { append: appendedInstructions } : {}
    };
    if (process.env.CLAUDE_CODEX_CLI) opts.pathToClaudeCodeExecutable = process.env.CLAUDE_CODEX_CLI;
    void sdk;
    return opts;
  }
  makeCanUseTool(context, autoAllow) {
    return async (toolName, input, options) => {
      const toolUseId = options.toolUseID || `tool-${newId()}`;
      const pending = this.turns.get(context.turnId);
      if (!pending) return { behavior: "deny", message: "turn already finished" };
      if (toolName === "AskUserQuestion") {
        try {
          const requestId2 = `${context.threadId}:${context.turnId}:askq:${toolUseId}`;
          const questions = parseAskUserQuestions(input);
          if (questions.length === 0) {
            return { behavior: "deny", message: "no questions provided" };
          }
          if (typeof pending.handlers.onUserInputRequest !== "function") {
            return { behavior: "deny", message: "user input not available in this runtime" };
          }
          const answers = await pending.handlers.onUserInputRequest({
            type: "user_input_request",
            requestId: requestId2,
            toolUseId,
            questions
          });
          const formatted = formatAskUserQuestionAnswers(input, questions, answers);
          return { behavior: "deny", message: formatted };
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          return { behavior: "deny", message: `AskUserQuestion failed: ${msg}` };
        }
      }
      if (autoAllow) return { behavior: "allow" };
      const requestId = `${context.threadId}:${context.turnId}:${toolName}:${toolUseId}`;
      const decision = await new Promise((resolve5) => {
        this.permissions.set(requestId, { resolve: resolve5 });
        void Promise.resolve(
          pending.handlers.onPermissionRequest({
            type: "permission_request",
            requestId,
            toolUseId,
            toolName,
            input
          })
        ).then((d) => {
          this.permissions.delete(requestId);
          resolve5(d);
        });
      });
      if (decision.decision === "accept" || decision.decision === "acceptForSession") {
        return { behavior: "allow", updatedInput: decision.updatedInput ?? input };
      }
      return { behavior: "deny", message: "denied by user" };
    };
  }
  async consume(pending) {
    const { context, handlers, query } = pending;
    try {
      for await (const message of query) {
        await this.handleMessage(pending, message);
        if (pending.resolved) break;
      }
      if (!pending.resolved && pending.deferredResult) {
        if (this.hasPendingWorkflowTasks(pending)) await this.stopWorkflowTasks(pending);
        await this.finishDeferredResult(pending, true);
        if (!pending.resolved) return;
      }
      if (!pending.resolved) {
        throw new Error("Claude stream ended without a completion result. Task completion could not be confirmed.");
      }
    } catch (err) {
      if (!pending.resolved) {
        await this.stopWorkflowTasks(pending);
        pending.resolved = true;
        this.turns.delete(context.turnId);
        const error = err instanceof Error ? err : new Error(String(err));
        try {
          await handlers.onEvent({ type: "error", message: error.message });
        } catch {
        }
        pending.reject(error);
      }
    }
  }
  async handleMessage(pending, message) {
    const type = String(message.type ?? "");
    if (type === "stream_event" || type === "assistant" || (type === "user" &&
      Array.isArray(message.message?.content) && message.message.content.some(block => block.type === "tool_result"))) pending.sawWork = true;
    switch (type) {
      case "system":
        await this.handleSystem(pending, message);
        break;
      case "stream_event":
        await this.handleStreamEvent(pending, message);
        break;
      case "assistant":
        await this.handleAssistant(pending, message);
        break;
      case "user":
        await this.handleUser(pending, message);
        break;
      case "result":
        await this.handleResult(pending, message);
        break;
      default:
        await this.handleOther(pending, type, message);
    }
  }
  async handleSystem(pending, message) {
    const subtype = String(message.subtype ?? "");
    if (subtype === "init") {
      const sessionId = String(message.session_id ?? "");
      if (sessionId) await pending.handlers.onEvent({ type: "session", claudeSessionId: sessionId });
      return;
    }
    if (subtype === "permission_denied") {
      const toolName = String(message.tool_name ?? "tool");
      await pending.handlers.onEvent({
        type: "notice",
        level: "warning",
        message: `Permission denied for ${toolName}`
      });
      return;
    }
    if (subtype === "task_started" && isWorkflowBackgroundTask(message)) {
      const taskId = String(message.task_id ?? "");
      if (!taskId) return;
      const sourceToolUseId = String(message.tool_use_id ?? "");
      if (message.skip_transcript === true) {
        const skipped = pending.skippedWorkflowTaskIds ?? (pending.skippedWorkflowTaskIds = /* @__PURE__ */ new Set());
        skipped.add(taskId);
        this.discardDeferredWorkflowLaunches(pending, taskId, sourceToolUseId);
        const hiddenState = pending.workflowTasks?.get(taskId);
        if (hiddenState) {
          hiddenState.terminal = true;
          await hiddenState.monitor?.stop();
          await this.closeWorkflowAgentLifecycles(
            pending,
            hiddenState,
            "Workflow transcript was hidden before the agent completed.",
            false
          );
          await this.closeWorkflowAggregateLifecycle(
            pending,
            hiddenState,
            "Workflow transcript was hidden.",
            false
          );
        }
        await this.finishDeferredResult(pending);
        return;
      }
      const toolUseId = workflowTaskToolUseId(taskId);
      if (pending.completedWorkflowTasks.has(toolUseId)) return;
      const state = this.ensureWorkflowTask(pending, taskId);
      if (sourceToolUseId && !state.toolUseId) state.toolUseId = sourceToolUseId;
      state.workflowName = String(message.workflow_name ?? "").trim() || state.workflowName;
      state.description = String(message.description ?? "").trim() || state.description;
      state.prompt = String(message.prompt ?? "").trim() || state.prompt || state.description || state.workflowName;
      this.attachDeferredWorkflowJournal(pending, state, sourceToolUseId);
      return;
    }
    if (subtype === "task_notification") {
      const taskId = String(message.task_id ?? "");
      if (!taskId) return;
      const knownState = pending.workflowTasks?.get(taskId);
      const hasWorkflowHint = isWorkflowBackgroundTask(message) || String(message.workflow_name ?? "").trim().length > 0;
      const sourceToolUseId = String(message.tool_use_id ?? "");
      const hasPendingWorkflowLaunch = [...pending.workflowLaunches?.values() ?? []].some(
        (launch) => launch.taskId === taskId
      );
      const hasPendingWorkflowTool = sourceToolUseId.length > 0 && pending.workflowToolUseIds?.has(sourceToolUseId) === true;
      if (!knownState && !hasWorkflowHint && !hasPendingWorkflowLaunch && !hasPendingWorkflowTool)
        return;
      if (pending.skippedWorkflowTaskIds?.has(taskId)) {
        this.discardDeferredWorkflowLaunches(pending, taskId, "");
        const hiddenState = pending.workflowTasks?.get(taskId);
        if (hiddenState) {
          hiddenState.terminal = true;
          await hiddenState.monitor?.stop();
          await this.closeWorkflowAgentLifecycles(
            pending,
            hiddenState,
            "Workflow transcript was hidden before the agent completed.",
            false
          );
          await this.closeWorkflowAggregateLifecycle(
            pending,
            hiddenState,
            "Workflow transcript was hidden.",
            false
          );
        }
        await this.finishDeferredResult(pending);
        return;
      }
      let state = knownState;
      if (!state) {
        state = this.ensureWorkflowTask(pending, taskId);
        if (sourceToolUseId && !state.toolUseId) state.toolUseId = sourceToolUseId;
        state.workflowName = String(message.workflow_name ?? "").trim() || state.workflowName;
        state.description = String(message.description ?? "").trim() || state.description;
        state.prompt = String(message.prompt ?? "").trim() || state.prompt || state.description || state.workflowName;
        this.attachDeferredWorkflowJournal(pending, state, sourceToolUseId);
      }
      if (message.skip_transcript === true) {
        const skipped = pending.skippedWorkflowTaskIds ?? (pending.skippedWorkflowTaskIds = /* @__PURE__ */ new Set());
        skipped.add(taskId);
        this.discardDeferredWorkflowLaunches(pending, taskId, state.toolUseId);
        state.terminal = true;
        await state.monitor?.stop();
        await this.closeWorkflowAgentLifecycles(
          pending,
          state,
          "Workflow transcript was hidden before the agent completed.",
          false
        );
        await this.closeWorkflowAggregateLifecycle(
          pending,
          state,
          "Workflow transcript was hidden.",
          false
        );
        await this.finishDeferredResult(pending);
        return;
      }
      const toolUseId = workflowTaskToolUseId(taskId);
      if (pending.completedWorkflowTasks.has(toolUseId)) {
        state.terminal = true;
        await this.finishDeferredResult(pending);
        return;
      }
      const status = String(message.status ?? "");
      const summary = String(message.summary ?? "").trim() || `Workflow ${status || "finished"}`;
      const usage = workflowTaskUsage(message.usage);
      const trailer = usage ? `
<usage>total_tokens: ${usage.totalTokens}
tool_uses: ${usage.toolUses}
duration_ms: ${usage.durationMs}</usage>` : "";
      const monitor = state.monitor;
      let monitorStopped = false;
      if (monitor && !state.aggregateStarted && !state.monitorFailed) {
        await settlesWithin2(monitor.flush(), WORKFLOW_TERMINAL_FLUSH_TIMEOUT_MS);
        if (state.monitor === monitor && !state.monitorFailed) {
          await monitor.drain(WORKFLOW_JOURNAL_SETTLE_TIMEOUT_MS);
        }
        if (state.monitor === monitor && !state.monitorFailed) {
          const projectedBeforeStop = this.workflowProjectedAgentIds(pending, state);
          if (monitor.startedCount > 0 || projectedBeforeStop.length > 0) {
            state.terminal = true;
          }
          await monitor.stop();
          monitorStopped = true;
          const projectedAgentIds = this.workflowProjectedAgentIds(pending, state);
          if (state.monitor === monitor) state.monitor = null;
          if (monitor.startedCount > 0 || projectedAgentIds.length > 0) {
            state.terminal = true;
            await this.closeWorkflowAgentLifecycles(
              pending,
              state,
              status === "completed" ? `Workflow completed before the individual transcript result became visible.
${summary}` : `Workflow ended before the agent published an individual result.
${summary}`,
              status !== "completed"
            );
            pending.completedWorkflowTasks.add(toolUseId);
            await pending.handlers.onEvent({
              type: "notice",
              level: status === "completed" ? "info" : "warning",
              message: `${state.workflowName || `Workflow ${taskId}`}: ${summary}${trailer}`
            });
            await this.finishDeferredResult(pending);
            return;
          }
        }
      }
      if (monitor && !monitorStopped && (state.monitor !== monitor || state.monitorFailed)) {
        await monitor.stop();
      }
      await this.ensureWorkflowAggregateStarted(pending, state);
      if (!pending.activeSubagents.delete(toolUseId)) return;
      pending.completedWorkflowTasks.add(toolUseId);
      state.terminal = true;
      await pending.handlers.onEvent({
        type: "tool_result",
        toolUseId,
        content: `${summary}${trailer}`,
        isError: status !== "completed"
      });
      await this.finishDeferredResult(pending);
    }
  }
  async handleStreamEvent(pending, message) {
    const event = message.event;
    if (!event) return;
    if (message.parent_tool_use_id || pending.activeSubagents.size > 0) return;
    const eventType = String(event.type ?? "");
    if (eventType === "message_start") {
      const inner = event.message;
      await this.beginAssistantMessage(pending, stringOrNull(inner?.id), true);
      return;
    }
    if (eventType === "content_block_start") {
      const block = event.content_block;
      if (block && String(block.type) === "tool_use") {
        const id = String(block.id ?? "");
        if (id) pending.toolStartSeen.add(id);
      }
      return;
    }
    if (eventType === "content_block_delta") {
      const delta = event.delta;
      if (!delta) return;
      const deltaType = String(delta.type ?? "");
      if (deltaType === "text_delta") {
        const text = String(delta.text ?? "");
        if (!text) return;
        this.recordStreamedBlock(pending, event.index, "text", text);
        if (pending.context.outputFormat) {
          pending.structuredBuffer += text;
          return;
        }
        await pending.handlers.onEvent({ type: "text_delta", delta: text });
      } else if (deltaType === "thinking_delta") {
        const thinking = String(delta.thinking ?? "");
        if (!thinking) return;
        this.recordStreamedBlock(pending, event.index, "thinking", thinking);
        await pending.handlers.onEvent({ type: "reasoning_delta", delta: thinking });
      }
    }
  }
  async beginAssistantMessage(pending, messageId, streamStart = false) {
    if (messageId ? messageId === pending.assistantMessageId : !streamStart) return;
    pending.assistantMessageId = messageId;
    pending.streamedBlocks = /* @__PURE__ */ new Map();
    if (!pending.context?.outputFormat) {
      await pending.handlers.onEvent({ type: "message_boundary" });
    }
  }
  recordStreamedBlock(pending, index, type, text) {
    const blockIndex = typeof index === "number" ? index : 0;
    const blocks = pending.streamedBlocks ??= /* @__PURE__ */ new Map();
    const previous = blocks.get(blockIndex);
    blocks.set(blockIndex, { type, text: (previous?.type === type ? previous.text : "") + text });
  }
  unstreamedBlockText(pending, type, text) {
    for (const [index, block] of pending.streamedBlocks ?? []) {
      if (block.type !== type || !text.startsWith(block.text)) continue;
      pending.streamedBlocks.delete(index);
      return text.slice(block.text.length);
    }
    return text;
  }
  async handleAssistant(pending, message) {
    const inner = message.message;
    if (!inner) return;
    const nestedMessage = Boolean(message.parent_tool_use_id);
    if (!nestedMessage && pending.activeSubagents.size === 0) {
      await this.beginAssistantMessage(pending, stringOrNull(inner.id));
    }
    const content = inner.content || [];
    for (const block of content) {
      const blockType = String(block.type ?? "");
      if (blockType === "text") {
        if (nestedMessage || pending.activeSubagents.size > 0) continue;
        const text = this.unstreamedBlockText(pending, "text", String(block.text ?? ""));
        if (pending.context?.outputFormat) {
          pending.structuredBuffer += text;
        } else if (text) {
          await pending.handlers.onEvent({ type: "text_delta", delta: text });
        }
      } else if (blockType === "thinking") {
        if (nestedMessage || pending.activeSubagents.size > 0) continue;
        const thinking = this.unstreamedBlockText(pending, "thinking", String(block.thinking ?? ""));
        if (thinking) await pending.handlers.onEvent({ type: "reasoning_delta", delta: thinking });
      } else if (blockType === "tool_use") {
        const id = String(block.id ?? "");
        const name2 = String(block.name ?? "");
        const input = block.input || {};
        if (!id) continue;
        const parentSubagent = nestedMessage || pending.activeSubagents.size > 0;
        if (isSubagentTool(name2)) {
          pending.activeSubagents.add(id);
        }
        if (parentSubagent && !isSubagentTool(name2)) continue;
        if (name2 === "StructuredOutput") {
          continue;
        }
        if (name2 === "AskUserQuestion") {
          continue;
        }
        await pending.handlers.onEvent({ type: "tool_use", toolUseId: id, toolName: name2, input });
        if (isWorkflowTool(name2)) pending.workflowToolUseIds.add(id);
      }
    }
  }
  async handleUser(pending, message) {
    const workflowLaunchResult = message.tool_use_result ?? message.toolUseResult;
    let workflowLaunchAttached = false;
    const inner = message.message;
    if (!inner) return;
    const content = inner.content || [];
    const toolResultCount = content.filter((block) => String(block.type) === "tool_result").length;
    for (const block of content) {
      if (String(block.type) !== "tool_result") continue;
      const toolUseId = String(block.tool_use_id ?? "");
      if (!toolUseId) continue;
      const isWorkflowLaunch = pending.workflowToolUseIds?.delete(toolUseId) === true;
      const wasSubagent = pending.activeSubagents.delete(toolUseId);
      const isError = Boolean(block.is_error);
      const bodyContent = block.content;
      await pending.handlers.onEvent({
        type: "tool_result",
        toolUseId,
        content: bodyContent,
        isError
      });
      if (!workflowLaunchAttached && isWorkflowLaunch && toolResultCount === 1) {
        workflowLaunchAttached = true;
        this.attachWorkflowJournal(pending, workflowLaunchResult, toolUseId);
      }
      void wasSubagent;
    }
  }
  attachWorkflowJournal(pending, value, toolUseId) {
    const launch = parseWorkflowLaunchInfo(
      value,
      pending.workflowTranscriptRoots ?? defaultWorkflowTranscriptRoots(process.env)
    );
    if (!launch) return;
    if (pending.skippedWorkflowTaskIds?.has(launch.taskId)) return;
    const state = pending.workflowTasks?.get(launch.taskId);
    if (!state) {
      const boundState = [...pending.workflowTasks?.values() ?? []].find(
        (candidate) => candidate.toolUseId === toolUseId
      );
      if (boundState) return;
      const launches = pending.workflowLaunches ?? (pending.workflowLaunches = /* @__PURE__ */ new Map());
      launches.set(toolUseId, launch);
      return;
    }
    this.attachParsedWorkflowJournal(pending, state, launch, toolUseId);
  }
  attachParsedWorkflowJournal(pending, state, launch, toolUseId) {
    if (launch.taskId !== state.taskId) return;
    if (state.toolUseId && state.toolUseId !== toolUseId) return;
    if (!state.toolUseId) state.toolUseId = toolUseId;
    state.workflowName = launch.workflowName || state.workflowName;
    state.description = launch.summary || state.description;
    state.prompt = state.prompt || state.description || state.workflowName;
    if (state.terminal || state.aggregateStarted || state.monitor) return;
    state.monitor = this.createWorkflowMonitor(pending, state, launch);
    state.monitor.start();
  }
  attachDeferredWorkflowJournal(pending, state, sourceToolUseId) {
    const launches = pending.workflowLaunches;
    if (!launches || launches.size === 0) return;
    if (sourceToolUseId) {
      const launch2 = launches.get(sourceToolUseId);
      if (!launch2) return;
      launches.delete(sourceToolUseId);
      this.attachParsedWorkflowJournal(pending, state, launch2, sourceToolUseId);
      return;
    }
    const matches = [...launches.entries()].filter(([, launch2]) => launch2.taskId === state.taskId);
    if (matches.length !== 1) return;
    const match = matches[0];
    if (!match) return;
    const [toolUseId, launch] = match;
    launches.delete(toolUseId);
    this.attachParsedWorkflowJournal(pending, state, launch, toolUseId);
  }
  discardDeferredWorkflowLaunches(pending, taskId, sourceToolUseId) {
    const launches = pending.workflowLaunches;
    if (!launches) return;
    if (sourceToolUseId) launches.delete(sourceToolUseId);
    for (const [toolUseId, launch] of launches) {
      if (launch.taskId === taskId) launches.delete(toolUseId);
    }
  }
  createWorkflowMonitor(pending, state, launch) {
    return new WorkflowJournalMonitor({
      launch,
      onStarted: async (agent) => {
        if (state.terminal) return;
        const toolUseId = workflowAgentToolUseId(state.taskId, agent.agentId);
        if (pending.activeSubagents.has(toolUseId) || pending.completedWorkflowTasks.has(toolUseId)) {
          return;
        }
        pending.activeSubagents.add(toolUseId);
        try {
          await pending.handlers.onEvent({
            type: "tool_use",
            toolUseId,
            toolName: "Agent",
            input: {
              description: agent.description,
              prompt: agent.prompt,
              subagent_type: "workflow"
            }
          });
        } catch (error) {
          pending.activeSubagents.delete(toolUseId);
          throw error;
        }
      },
      onResult: async (agent) => {
        if (state.terminal) return;
        const toolUseId = workflowAgentToolUseId(state.taskId, agent.agentId);
        if (!pending.activeSubagents.has(toolUseId)) return;
        await pending.handlers.onEvent({
          type: "tool_result",
          toolUseId,
          content: agent.content,
          isError: agent.isError
        });
        pending.activeSubagents.delete(toolUseId);
        pending.completedWorkflowTasks.add(toolUseId);
      },
      onError: async (error) => {
        if (state.terminal) return;
        const message = `Workflow monitoring failed: ${error.message}`;
        state.monitorFailed = true;
        state.monitor = null;
        state.terminal = true;
        pending.workflowFailure ??= message;
        await this.closeWorkflowAgentLifecycles(pending, state, message, true);
        await this.closeWorkflowAggregateLifecycle(pending, state, message, true);
        if (state.toolUseId) pending.workflowToolUseIds.delete(state.toolUseId);
        await pending.handlers.onEvent({
          type: "notice",
          level: "warning",
          message
        });
        if (pending.deferredResult) {
          pending.completedWorkflowTasks.add(workflowTaskToolUseId(state.taskId));
          pending.deferredResult = {
            ...pending.deferredResult,
            success: false,
            resultText: pending.deferredResult.resultText ?? message
          };
          await this.finishDeferredResult(pending, true);
        }
      }
    });
  }
  workflowProjectedAgentIds(pending, state) {
    const agentIds = new Set(state.monitor?.activeAgentIds() ?? []);
    const prefix = `workflow-agent:${state.taskId}:`;
    for (const toolUseId of pending.activeSubagents) {
      if (!toolUseId.startsWith(prefix)) continue;
      const agentId = toolUseId.slice(prefix.length);
      if (agentId) agentIds.add(agentId);
    }
    return [...agentIds];
  }
  async closeWorkflowAgentLifecycles(pending, state, message, isError) {
    let closed = 0;
    for (const agentId of this.workflowProjectedAgentIds(pending, state)) {
      const toolUseId = workflowAgentToolUseId(state.taskId, agentId);
      if (!pending.activeSubagents.delete(toolUseId)) continue;
      pending.completedWorkflowTasks.add(toolUseId);
      await pending.handlers.onEvent({
        type: "tool_result",
        toolUseId,
        content: `${message}
agentId: ${agentId}`,
        isError
      });
      closed += 1;
    }
    return closed;
  }
  async closeWorkflowAggregateLifecycle(pending, state, message, isError) {
    const toolUseId = workflowTaskToolUseId(state.taskId);
    if (!pending.activeSubagents.delete(toolUseId)) return false;
    pending.completedWorkflowTasks.add(toolUseId);
    await pending.handlers.onEvent({
      type: "tool_result",
      toolUseId,
      content: message,
      isError
    });
    return true;
  }
  ensureWorkflowTask(pending, taskId) {
    const tasks = pending.workflowTasks ?? (pending.workflowTasks = /* @__PURE__ */ new Map());
    const existing = tasks.get(taskId);
    if (existing) return existing;
    const state = {
      taskId,
      toolUseId: "",
      workflowName: "",
      description: "",
      prompt: "",
      monitor: null,
      aggregateStarted: false,
      terminal: false
    };
    tasks.set(taskId, state);
    return state;
  }
  async ensureWorkflowAggregateStarted(pending, state) {
    if (state.terminal || state.aggregateStarted) return;
    const toolUseId = workflowTaskToolUseId(state.taskId);
    if (pending.completedWorkflowTasks.has(toolUseId)) return;
    state.aggregateStarted = true;
    pending.activeSubagents.add(toolUseId);
    try {
      await pending.handlers.onEvent({
        type: "tool_use",
        toolUseId,
        toolName: "Agent",
        input: {
          description: state.workflowName ? `Workflow: ${state.workflowName}` : "Workflow",
          prompt: state.prompt || state.description || state.workflowName || "Workflow",
          subagent_type: "workflow"
        }
      });
    } catch (error) {
      state.aggregateStarted = false;
      pending.activeSubagents.delete(toolUseId);
      throw error;
    }
  }
  async stopWorkflowTasks(pending) {
    const tasks = pending.workflowTasks;
    if (tasks) {
      for (const state of tasks.values()) {
        state.terminal = true;
        await state.monitor?.stop();
        await this.closeWorkflowAgentLifecycles(
          pending,
          state,
          "Workflow monitoring stopped before the agent published an individual result.",
          true
        );
        await this.closeWorkflowAggregateLifecycle(
          pending,
          state,
          "Workflow monitoring stopped before the workflow published a terminal result.",
          true
        );
      }
    }
    pending.workflowLaunches?.clear();
  }
  hasPendingWorkflowTasks(pending) {
    if ((pending.workflowToolUseIds?.size ?? 0) > 0) return true;
    if ((pending.workflowLaunches?.size ?? 0) > 0) return true;
    for (const state of pending.workflowTasks?.values() ?? []) {
      if (!state.terminal) return true;
    }
    return false;
  }
  async finishDeferredResult(pending, force = false) {
    const deferred = pending.deferredResult;
    if (!deferred || pending.resolved) return;
    if (!force && deferred.success && this.hasPendingWorkflowTasks(pending)) return;
    pending.deferredResult = null;
    pending.resolved = true;
    this.turns.delete(pending.context.turnId);
    try {
      await pending.handlers.onEvent({
        type: "completed",
        success: deferred.success,
        result: deferred.resultText,
        claudeSessionId: deferred.claudeSessionId
      });
      if (deferred.success) {
        pending.resolve();
      } else {
        pending.reject(new Error(deferred.resultText ?? "Claude turn failed"));
      }
    } catch (err) {
      pending.reject(err instanceof Error ? err : new Error(String(err)));
    }
  }
  async handleResult(pending, message) {
    if (pending.deferredResult || pending.resolved) return;
    const subtype = String(message.subtype ?? "");
    const success = subtype === "success" && !message.is_error && pending.workflowFailure == null;
    const resultText2 = pending.workflowFailure ?? (message.result == null ? null : String(message.result));
    const claudeSessionId = message.session_id == null ? null : String(message.session_id);
    if (success && !resultText2?.trim() && !pending.sawWork && !this.hasPendingWorkflowTasks(pending)) {
      if (message.num_turns === 0 && message.duration_api_ms === 0) {
        const error = new Error("Claude completed without starting model work."); error.name = "empty_turn"; throw error;
      }
      throw new Error("Claude returned no response. Task completion could not be confirmed.");
    }
    const usage = message.usage || {};
    if (Object.keys(usage).length > 0) {
      await pending.handlers.onEvent({ type: "usage", usage });
    }
    await pending.handlers.onEvent({
      type: "metrics",
      durationMs: numberOrNull(message.duration_ms),
      apiDurationMs: numberOrNull(message.duration_api_ms),
      numTurns: numberOrNull(message.num_turns),
      costUsd: numberOrNull(message.total_cost_usd)
    });
    if (pending.context.outputFormat && pending.structuredBuffer) {
      await pending.handlers.onEvent({ type: "text_delta", delta: pending.structuredBuffer.trim() });
    }
    pending.deferredResult = { success, resultText: resultText2, claudeSessionId };
    if (!success) await this.stopWorkflowTasks(pending);
    await this.finishDeferredResult(pending);
  }
  async handleOther(pending, type, message) {
    if (type === "rate_limit" || type === "rate_limit_event") {
      const info = message.rate_limit_info;
      if (info) await pending.handlers.onEvent({ type: "rate_limit", info });
      if (info?.status === "allowed") return;
      const explicit = stringOrNull(message.message);
      if (!explicit && info?.status !== "allowed_warning" && info?.status !== "rejected") {
        if (type === "rate_limit_event") return;
      }
      await pending.handlers.onEvent({
        type: "notice",
        level: "warning",
        message: explicit ?? rateLimitNotice(info)
      });
      return;
    }
    if (type === "hook" || type === "hook_event" || type === "system_hook_event") {
      const hookName = String(message.hook_event_name ?? message.hook_name ?? "hook");
      const status = stringOrNull(message.status) ?? stringOrNull(message.subtype);
      const decision = stringOrNull(message.decision) ?? stringOrNull(message.permission_decision);
      const text = stringOrNull(message.message) ?? stringOrNull(message.reason);
      await pending.handlers.onEvent({
        type: "hook",
        hookName,
        status,
        decision,
        message: text
      });
    }
  }
};
function rateLimitNotice(info) {
  const window = String(info?.rateLimitType ?? "usage").replaceAll("_", " ");
  const status = info?.status === "allowed_warning" ? `Claude usage is nearing the ${window} limit` : `Claude ${window} limit reached`;
  const utilization = typeof info?.utilization === "number" ? info.utilization : null;
  const used = utilization == null ? "" : ` (${Math.round(utilization * 100)}% used)`;
  const resetsAt = typeof info?.resetsAt === "number" ? new Date(info.resetsAt * 1e3) : null;
  const reset = resetsAt && Number.isFinite(resetsAt.getTime()) ? ` Resets at ${resetsAt.toISOString()}.` : "";
  return `${status}${used}.${reset}`;
}
function derivePermissionMode(approvalPolicy, sandboxMode, planMode) {
  const envOverride = configuredPermissionMode();
  if (envOverride) return envOverride;
  if (planMode) return "plan";
  if (sandboxMode === "danger-full-access" || approvalPolicy === "never") return "default";
  if (approvalPolicy === "on-failure") return "acceptEdits";
  return "default";
}
function configuredPermissionMode() {
  const value = process.env.CLAUDE_CODEX_PERMISSION_MODE;
  if (value === "default" || value === "acceptEdits" || value === "bypassPermissions" || value === "plan" || value === "dontAsk" || value === "auto") {
    return value;
  }
  return null;
}
function isSubagentTool(name2) {
  const n = name2.trim().toLowerCase();
  return n === "task" || n === "agent" || n === "subagent" || n === "spawn_agent" || n === "spawnagent";
}
function isWorkflowTool(name2) {
  return name2 === "Workflow";
}
function isWorkflowBackgroundTask(message) {
  return String(message.task_type ?? "") === "local_workflow";
}
function workflowTaskToolUseId(taskId) {
  return `workflow-task:${taskId}`;
}
function workflowAgentToolUseId(taskId, agentId) {
  return `workflow-agent:${taskId}:${agentId}`;
}
async function settlesWithin2(promise, timeoutMs) {
  if (timeoutMs <= 0) return false;
  let timer = null;
  try {
    return await Promise.race([
      promise.then(() => true),
      new Promise((resolveTimeout) => {
        timer = setTimeout(() => resolveTimeout(false), timeoutMs);
      })
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}
function workflowTaskUsage(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const usage = value;
  const totalTokens = numberOrNull(usage.total_tokens);
  const toolUses = numberOrNull(usage.tool_uses);
  const durationMs = numberOrNull(usage.duration_ms);
  if (totalTokens === null || toolUses === null || durationMs === null) return null;
  return { totalTokens, toolUses, durationMs };
}
function numberOrNull(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}
function stringOrNull(value) {
  return typeof value === "string" && value.length > 0 ? value : null;
}
function sdkResumeSessionId(value, cwd) {
  if (!value) return null;
  if (/^(agent-http|agentapi|claude-p):/.test(value)) return null;
  if (cwd) {
    const home = homedir();
    const slug = (dir) => dir.replace(/[^a-zA-Z0-9-]/g, "-");
    const slugs = [slug(cwd)];
    try {
      const real = realpathSync2(cwd);
      if (real !== cwd) slugs.push(slug(real));
    } catch {
    }
    const configRoots = /* @__PURE__ */ new Set([join10(home, ".claude")]);
    if (process.env.CLAUDE_CONFIG_DIR) configRoots.add(process.env.CLAUDE_CONFIG_DIR);
    const extraRoots = (process.env.CLAUDE_CONFIG_DIRS || process.env.CLAUDE_ROUTER_DIR || "").split(/[,:]/).map((p) => p.trim()).filter(Boolean);
    for (const extra of extraRoots) {
      if (existsSync7(extra)) configRoots.add(extra);
    }
    const cacheBase = join10(home, ".cache");
    if (existsSync7(cacheBase)) {
      try {
        for (const sub of readdirSync3(cacheBase)) {
          const routerDir = join10(cacheBase, sub, "claude-router");
          if (existsSync7(routerDir)) {
            for (const item of readdirSync3(routerDir)) {
              configRoots.add(join10(routerDir, item));
            }
          }
        }
      } catch {
      }
    }
    let sourceFile = null;
    for (const root of configRoots) {
      for (const s of slugs) {
        const p = join10(root, "projects", s, value + ".jsonl");
        if (existsSync7(p)) {
          sourceFile = p;
          break;
        }
      }
      if (sourceFile) break;
      const sessFile = join10(root, "sessions", value + ".jsonl");
      if (existsSync7(sessFile)) {
        sourceFile = sessFile;
        break;
      }
    }
    if (!sourceFile) return null;
    for (const root of configRoots) {
      for (const s of slugs) {
        const targetDir = join10(root, "projects", s);
        const targetFile = join10(targetDir, value + ".jsonl");
        if (targetFile === sourceFile) continue;
        try {
          if (!existsSync7(targetDir)) mkdirSync4(targetDir, { recursive: true });
          if (!existsSync7(targetFile)) {
            copyFileSync(sourceFile, targetFile);
          }
        } catch {
        }
      }
    }
  }
  return value;
}
function parseAskUserQuestions(input) {
  const raw = input.questions || [];
  const out = [];
  for (let i = 0; i < raw.length; i++) {
    const q = raw[i] || {};
    const header = String(q.header ?? `Question ${i + 1}`);
    const question = String(q.question ?? "");
    const optionsRaw = q.options || [];
    const options = optionsRaw.map((o) => ({
      label: String(o.label ?? ""),
      description: String(o.description ?? "")
    }));
    options.push({ label: "Other", description: "Provide a custom answer" });
    out.push({
      id: `q${i}`,
      header: header.slice(0, 12),
      question,
      isOther: false,
      isSecret: false,
      options
    });
  }
  return out;
}
function formatAskUserQuestionAnswers(input, questions, answers) {
  const original = input.questions || [];
  const answersByQuestion = {};
  const annotations = {};
  for (const [i, q] of questions.entries()) {
    const origQ = original[i] || {};
    const questionText = String(origQ.question ?? q.question);
    const slot = answers.answers[q.id];
    if (!slot) {
      answersByQuestion[questionText] = "";
      continue;
    }
    const picked = Array.isArray(slot.answers) ? slot.answers.filter((s) => s !== "Other") : [];
    const notes = typeof slot.notes === "string" ? slot.notes : null;
    if (picked.length === 0 && notes) {
      answersByQuestion[questionText] = notes;
    } else if (notes && picked.includes("Other")) {
      answersByQuestion[questionText] = notes;
    } else {
      answersByQuestion[questionText] = picked.join(", ");
    }
    if (notes) annotations[questionText] = { notes };
  }
  const payload = {
    questions: original,
    answers: answersByQuestion,
    ...Object.keys(annotations).length > 0 ? { annotations } : {}
  };
  return JSON.stringify(payload);
}
export {
  CodexClaudeAppServer,
  NativeClaudeRuntime,
  SessionStore
};
