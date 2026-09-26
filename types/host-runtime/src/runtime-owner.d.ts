import type { Host } from "../../contracts/src/generated.js";
/**
 * The single local owner for one configured runtime target.
 *
 * Deployment accepts and records targets; this small loop turns one Windows
 * process target into one child process, checks readiness and applies the
 * declared restart policy. Docker and Linux systemd have their own owners and
 * never enter this class.
 */
export declare class RuntimeOwner {
    readonly instanceId: string;
    readonly bootstrapRoot: string;
    private readonly lock;
    private readonly journal;
    private readonly bootId;
    private readonly controller;
    private task;
    private child;
    private childResult;
    private active;
    private config;
    private manifest;
    private restarts;
    private nextRestart;
    private startedAt;
    private unreadySince;
    private lastReady;
    private checkAt;
    private restartAllowed;
    private restartCode;
    private blocked;
    private closed;
    private lastObservation;
    get host(): Host.HostConfig;
    get completion(): Promise<void>;
    constructor(host: Host.HostConfig, instanceId: string, bootstrapRoot: string);
    start(): void;
    close(): Promise<void>;
    private observe;
    private stopChild;
    private select;
    private launch;
    private secrets;
    private backoff;
    private tick;
    private run;
}
