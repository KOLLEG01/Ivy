// Frozen original adapter declarations; test oracle only.
// MIT: services/agent-manager/claude/LICENSE.adapter.
import {existsSync,readFileSync,renameSync,writeFileSync,mkdirSync,lstatSync,realpathSync} from 'node:fs';
import {join,dirname,resolve,isAbsolute as nativeAbsolute,win32,posix} from 'node:path';
import assert from 'node:assert/strict';
export const sourceIdentity={"commit":"fb33a8c2181a96a1642855d6ae5cf583058e03c2","patchHash":"sha256:25fb871703801d7f57ebe600ac485a904c61fd44efeb2e57730959043f6eb0f5","sources":[{"file":"src/projects.mts","sha256":"746666a147527b767919850d7a683023362b6d3c7f4f9d3e7783282ffc56855c","bytes":7345},{"file":"src/model-discovery.mts","sha256":"29c50c170cf17931fe4f1c22c192bd0747c50d5766fa245229d8b828e43b63cf","bytes":2014},{"file":"src/rate-limits.mts","sha256":"7de7f0da92ab61c6f77379816f6ee219ca94da992679a5fa45cec1066a4055b0","bytes":5357}],"typescript":"5.9.3"};
const RealDate=Date;
export function reference(root,state){
 assert(nativeAbsolute(root));assert(lstatSync(root).isDirectory()&&!lstatSync(root).isSymbolicLink());assert.equal(resolve(realpathSync(root)),resolve(root));
 const adapterHome=()=>root;
 const ensureParent=path=>{assert.equal(dirname(path),root);mkdirSync(dirname(path),{recursive:true});};
 const isAbsolute=state.style==='posix'?posix.isAbsolute:win32.isAbsolute;
 const normalize=state.style==='posix'?posix.normalize:win32.normalize;
 const Date=class extends RealDate{static now(){state.clockCalls++;return state.now;}};
 const nowSeconds=()=>Math.floor(Date.now()/1000);
 const newId=()=>{const id=state.ids[state.idCalls++];assert.equal(typeof id,'string','Fixture id exhausted');return id;};
function name(value) {
    if (typeof value !== 'string' || !value.trim())
        throw new Error('Project name is required');
    return value.trim();
}
function roots(value) {
    if (!Array.isArray(value))
        throw new Error('Project roots must be an array');
    return value.map((root) => {
        const path = root?.path;
        if (typeof path !== 'string' || !isAbsolute(path))
            throw new Error('Project roots must be absolute paths');
        return { path: normalize(path) };
    });
}
function metadata(value) {
    if (value == null)
        return {};
    if (typeof value !== 'object' ||
        Array.isArray(value) ||
        Object.values(value).some((entry) => typeof entry !== 'string'))
        throw new Error('Project metadata must contain strings');
    return Object.fromEntries(Object.entries(value).sort(([a], [b]) => a.localeCompare(b)));
}
class ProjectStore {
    path = join(adapterHome(), 'projects.json');
    state;
    ephemeral = new Map();
    thread;
    constructor(thread) {
        this.thread = thread;
        this.state = existsSync(this.path)
            ? JSON.parse(readFileSync(this.path, 'utf8'))
            : { projects: [], requests: {}, threads: {} };
    }
    save(next) {
        ensureParent(this.path);
        const temporary = this.path + '.tmp';
        writeFileSync(temporary, JSON.stringify(next) + '\n', { mode: 0o600 });
        renameSync(temporary, this.path);
        this.state = next;
    }
    summary(project) {
        let recencyAt = null;
        for (const [id, projectId] of Object.entries(this.state.threads)) {
            if (projectId !== project.id)
                continue;
            const thread = this.thread(id);
            if (thread && !thread.archived && !thread.ephemeral)
                recencyAt = Math.max(recencyAt ?? 0, thread.updatedAt);
        }
        return { ...structuredClone(project), recencyAt };
    }
    read(id) {
        const project = this.state.projects.find((entry) => entry.id === id);
        if (!project)
            throw new Error('Unknown project: ' + String(id));
        return this.summary(project);
    }
    list(params) {
        if (params.sortDirection != null && params.sortKey == null)
            throw new Error('Project sort direction requires a sort key');
        const recency = params.sortKey === 'recencyAt';
        const direction = params.sortDirection === 'asc'
            ? 1
            : params.sortDirection === 'desc'
                ? -1
                : recency
                    ? -1
                    : 1;
        const projects = this.state.projects
            .map((entry) => this.summary(entry))
            .sort((a, b) => {
            if (recency && (a.recencyAt === null || b.recencyAt === null))
                return a.recencyAt === b.recencyAt
                    ? a.position - b.position
                    : a.recencyAt === null
                        ? 1
                        : -1;
            return (direction *
                (recency ? a.recencyAt - b.recencyAt : a.position - b.position) ||
                a.id.localeCompare(b.id));
        });
        const offset = params.cursor == null ? 0 : Number(params.cursor);
        if (!Number.isSafeInteger(offset) || offset < 0)
            throw new Error('Invalid project cursor');
        const limit = Math.max(1, Math.min(Number(params.limit ?? 100), 200));
        return {
            data: projects.slice(offset, offset + limit),
            nextCursor: offset + limit < projects.length ? String(offset + limit) : null,
        };
    }
    create(params) {
        if (typeof params.idempotencyKey !== 'string' || !params.idempotencyKey)
            throw new Error('Project creation requires an idempotency key');
        const input = {
            name: name(params.name),
            roots: roots(params.roots),
            metadata: metadata(params.metadata),
        };
        const fingerprint = JSON.stringify(input);
        const previous = Object.hasOwn(this.state.requests, params.idempotencyKey)
            ? this.state.requests[params.idempotencyKey]
            : undefined;
        if (previous) {
            if (previous.fingerprint !== fingerprint)
                throw new Error('Project idempotency key belongs to another request');
            return this.read(previous.projectId);
        }
        const now = nowSeconds();
        const project = {
            ...input,
            id: newId(),
            position: Math.max(-1, ...this.state.projects.map((entry) => entry.position)) + 1,
            createdAt: now,
            updatedAt: now,
        };
        const next = structuredClone(this.state);
        next.projects.push(project);
        next.requests = {
            ...next.requests,
            [params.idempotencyKey]: { fingerprint, projectId: project.id },
        };
        this.save(next);
        return this.summary(project);
    }
    update(params) {
        const project = this.read(params.projectId);
        const next = structuredClone(this.state);
        const target = next.projects.find((entry) => entry.id === project.id);
        if (params.name != null)
            target.name = name(params.name);
        if (params.roots != null)
            target.roots = roots(params.roots);
        if (params.metadata != null)
            target.metadata = metadata(params.metadata);
        target.updatedAt = nowSeconds();
        this.save(next);
        return this.summary(target);
    }
    delete(id) {
        const project = this.read(id);
        const next = structuredClone(this.state);
        next.projects = next.projects.filter((entry) => entry.id !== project.id);
        for (const [threadId, projectId] of Object.entries(next.threads))
            if (projectId === project.id)
                delete next.threads[threadId];
        this.save(next);
        for (const [threadId, projectId] of this.ephemeral)
            if (projectId === project.id)
                this.ephemeral.delete(threadId);
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
        const seen = new Set();
        while (current && !seen.has(current.id)) {
            seen.add(current.id);
            const id = this.ephemeral.get(current.id) ?? this.state.threads[current.id];
            if (id)
                return id;
            current = current.forkedFromId ? this.thread(current.forkedFromId) : null;
        }
        return null;
    }
}

function runtimeModelList(models, configuredModel, configuredEffort) {
    const first = models[0];
    if (!first)
        throw new Error('Claude CLI returned no available models');
    const rows = new Map();
    const idOf = (model) => model.resolvedModel && !/\[.+\]$/.test(model.value) ? model.resolvedModel : model.value;
    const defaultId = idOf(models.find((model) => model.value === configuredModel || idOf(model) === configuredModel) ??
        models.find((model) => model.value === 'default') ??
        first);
    for (const model of models) {
        const id = idOf(model);
        if (!rows.has(id) || model.value !== 'default')
            rows.set(id, model);
    }
    return {
        data: [...rows].map(([id, model]) => {
            const efforts = model.supportsEffort === false ? [] : (model.supportedEffortLevels ?? []);
            const version = /^claude-[a-z][a-z-]*-(\d+(?:-\d+)*)$/
                .exec((model.resolvedModel ?? id).replace(/-\d{8}$/, ''))?.[1]
                ?.replaceAll('-', '.');
            return {
                id,
                model: id,
                upgrade: null,
                upgradeInfo: null,
                availabilityNux: null,
                displayName: version && !model.displayName.includes(version)
                    ? `${model.displayName} ${version}`
                    : model.displayName,
                description: model.description,
                hidden: false,
                supportedReasoningEfforts: efforts.map((reasoningEffort) => ({
                    reasoningEffort,
                    description: reasoningEffort,
                })),
                defaultReasoningEffort: efforts.includes(configuredEffort)
                    ? configuredEffort
                    : (efforts[0] ?? 'none'),
                inputModalities: ['text', 'image'],
                supportsPersonality: false,
                additionalSpeedTiers: [],
                isDefault: id === defaultId,
            };
        }),
        nextCursor: null,
    };
}

const WEEK_MINUTES = 7 * 24 * 60;
const LABELS = {
    seven_day_opus: 'Weekly · Opus',
    seven_day_sonnet: 'Weekly · Sonnet',
    seven_day_oauth_apps: 'Weekly · OAuth apps',
};
const percent = (value) => Math.min(100, Math.max(0, Math.round(value)));
const seconds = (value) => {
    if (typeof value === 'number' && Number.isFinite(value))
        return Math.round(value);
    if (typeof value !== 'string')
        return null;
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? Math.round(parsed / 1000) : null;
};
class RateLimitStore {
    path = join(adapterHome(), 'rate-limits.json');
    windows = {};
    usageAt = 0;
    constructor() {
        try {
            if (existsSync(this.path))
                this.windows = JSON.parse(readFileSync(this.path, 'utf8'));
        }
        catch {
            this.windows = {};
        }
    }
    /** Whether the on-demand `/usage` read is due; it runs at most once per interval. */
    usageDue(now = Date.now(), intervalMs = 60_000) {
        return now - this.usageAt >= intervalMs;
    }
    /** A `rate_limit_info` from a turn; its utilization is a fraction of the window. */
    recordEvent(info, now = Date.now()) {
        const type = typeof info?.rateLimitType === 'string' ? info.rateLimitType : '';
        if (!type || type === 'overage' || typeof info?.utilization !== 'number')
            return;
        this.save({
            ...this.windows,
            [type]: {
                usedPercent: percent(info.utilization * 100),
                resetsAt: seconds(info.resetsAt),
                observedAt: now,
                ...(info.status === 'rejected' ? { rejected: true } : {}),
            },
        });
    }
    /** The `rate_limits` of the structured `/usage` data; utilization is already a percentage. */
    recordUsage(limits, now = Date.now()) {
        this.usageAt = now;
        if (!limits)
            return;
        const next = { ...this.windows };
        const add = (key, value) => {
            const window = value;
            if (typeof window?.utilization !== 'number')
                return;
            next[key] = { usedPercent: percent(window.utilization), resetsAt: seconds(window.resets_at), observedAt: now };
        };
        for (const key of ['five_hour', 'seven_day', 'seven_day_opus', 'seven_day_sonnet', 'seven_day_oauth_apps'])
            add(key, limits[key]);
        if (Array.isArray(limits.model_scoped))
            for (const entry of limits.model_scoped)
                if (typeof entry?.display_name === 'string' && entry.display_name)
                    add('model:' + entry.display_name, entry);
        this.save(next);
    }
    /** The Codex response; a window whose reset has passed starts again at zero. */
    read(nowSeconds = Math.floor(Date.now() / 1000)) {
        const window = (key, minutes) => {
            const value = this.windows[key];
            if (!value)
                return null;
            const expired = value.resetsAt !== null && value.resetsAt <= nowSeconds;
            return { usedPercent: expired ? 0 : value.usedPercent, windowDurationMins: minutes, resetsAt: expired ? null : value.resetsAt };
        };
        const reached = Object.values(this.windows).some((value) => value.rejected && (value.resetsAt === null || value.resetsAt > nowSeconds));
        const bucket = (limitId, limitName, primary, secondary = null) => ({
            limitId,
            limitName,
            primary,
            secondary,
            credits: null,
            planType: null,
            rateLimitReachedType: reached && limitId === 'claude-code' ? 'rate_limit_reached' : null,
        });
        const rateLimits = bucket('claude-code', 'Claude Code', window('five_hour', 300), window('seven_day', WEEK_MINUTES));
        const byId = { 'claude-code': rateLimits };
        for (const key of Object.keys(this.windows).sort()) {
            if (key === 'five_hour' || key === 'seven_day')
                continue;
            const name = LABELS[key] ?? (key.startsWith('model:') ? 'Weekly · ' + key.slice(6) : null);
            if (name)
                byId[key] = bucket(key, name, window(key, WEEK_MINUTES));
        }
        return { rateLimits, rateLimitsByLimitId: byId };
    }
    save(next) {
        this.windows = next;
        try {
            ensureParent(this.path);
            const temporary = this.path + '.tmp';
            writeFileSync(temporary, JSON.stringify(next) + '\n', { mode: 0o600 });
            renameSync(temporary, this.path);
        }
        catch {
            // Usage remains available in memory when the adapter home is not writable.
        }
    }
}

 return {ProjectStore,RateLimitStore,runtimeModelList};
}
