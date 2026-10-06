import { uiContentPolicy } from './ui-security.js';
import { browserAppManifest, browserAppWorker } from './browser-app.js';
import { BrowserPush } from './browser-push.js';
import { dashboardImageHttp } from './dashboard-image.js';
import type { PushSender } from './browser-push.js';
import type { BrowserPushRequest } from './browser-push-store.js';
import { reservedUiSlugs } from '../../../packages/contracts/src/ui-route.js';
import { loginPage, loginThemeScript } from "./login-page.js";
import {
  connectionInFlightRequests,
  managementFrameBytes,
  managementSocketBufferBytes,
} from "../../../packages/contracts/src/limits.js";
import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse, Server } from "node:http";
import type { Socket } from "node:net";
import { isIP } from "node:net";
import { randomUUID } from "node:crypto";
import { readFile, stat, realpath } from "node:fs/promises";
import { createReadStream } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { WebSocketServer, WebSocket } from "ws";
import { digest } from "../../../packages/contracts/src/canonical.js";
import { encodeJson } from "../../../packages/contracts/src/canonical-json.js";
import {
  IvyError,
  requireThat,
  fail,
} from "../../../packages/contracts/src/errors.js";
import type { WireError } from "../../../packages/contracts/src/errors.js";
import {
  validateInput,
  validateOutput,
  validateRequest,
  validateResponseFrame,
} from "../../../packages/contracts/src/core-validation.js";
import {
  validateServiceNotificationFrame,
  validateToolRouting,
} from "../../../packages/contracts/src/transport-validation.js";
import type {
  Host,
  Operation,
  Transport,
} from "../../../packages/contracts/src/generated.js";
import { assetPath } from "./uis.js";
import { rpcFailure, validRequestId } from "./kernel.js";
import type {
  ConnectionContext,
  KernelOptions,
  KernelResult,
} from "./kernel.js";
import type { AuthenticatedContext } from "./store.js";
import type { PreparedCall } from "./live-routing.js";
import { WorkerClient } from "./worker-client.js";
import { handleMcp } from "./mcp.js";
import type { McpCallObservation } from "./mcp.js";
import { mcpIconSvg } from "./mcp-branding.js";
import {
  DispatchGate,
  RequestBudget,
  AuthenticationRate,
} from "./admission.js";
import type { RequestLease } from "./admission.js";
import { LiveRouting } from "./live-routing.js";
import { HiveOAuth, oauthPage } from "./oauth.js";
import { PackageRegistry } from "./packages.js";
import { UiFiles } from "./ui-files.js";
import { resolveCoreCall } from "./discovery.js";
import { readUiBundle } from "../../../packages/cli/src/publish-ui.js";
import { newOperationId } from "../../../packages/sdk/src/client.js";
import { deriveOperationId } from "../../../packages/contracts/src/operation-id.js";
import type { RpcClient } from "../../../packages/sdk/src/client.js";

const MAX_FRAME = managementFrameBytes;
interface Peer {
  socket: WebSocket;
  context: ConnectionContext;
  chain: DispatchGate;
  active: number;
  alive: boolean;
  diagnosedAt: number;
  notificationFilters: Operation.NotificationFilter[];
  changeScopes: string[];
}
interface Forwarded {
  peer: Peer;
  call: PreparedCall;
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
}
export interface ServerOptions extends KernelOptions {
  listenHost?: string;
  listenPort?: number;
  consoleRoot?: string;
  callTimeoutMs?: number;
  trustedProxyAddresses?: string[];
  packageRoot?: string;
  packagePublisherPrincipalIds?: string[];
  observeMcpCall?: (observation: McpCallObservation) => void;
  sendBrowserPush?: PushSender;
}

export class HiveServer {
  readonly worker: WorkerClient;
  readonly server: Server;
  readonly base: URL;
  readonly basePath: string;
  private readonly ws = new WebSocketServer({
    noServer: true,
    maxPayload: MAX_FRAME,
    perMessageDeflate: false,
  });
  private readonly peers = new Set<Peer>();
  private readonly routing: LiveRouting;
  private readonly oauth: HiveOAuth;
  private readonly packages: PackageRegistry | null;
  private readonly uiFiles: UiFiles;
  private readonly browserPush: BrowserPush;
  private readonly forwarded = new Map<string, Forwarded>();
  private readonly httpActive = new Map<Socket, number>();
  private authenticatedSockets = new WeakMap<
    Socket,
    Map<string, Promise<ConnectionContext>>
  >();
  private bufferedBodies = 0;
  private healthTimer: NodeJS.Timeout | null = null;
  private recoveryTimer: NodeJS.Timeout | null = null;
  private readonly dispatchGate = new DispatchGate();
  private readonly requests = new RequestBudget();
  private readonly authentication = new RequestBudget(
    512,
    512 * 1024,
    64,
    64 * 1024,
    "authentication",
  );
  private readonly arrivals = new AuthenticationRate();
  private readonly connections = new RequestBudget(
    512,
    512 * 1024,
    connectionInFlightRequests,
    64 * 1024,
    "connection",
  );
  private readonly bodies = new WeakMap<IncomingMessage, RequestLease>();
  private connectionGeneration = 0;
  private eventsAvailableThroughSequence = 0;
  private eventsAvailabilityScheduled = false;
  private stopped = false;

  constructor(readonly options: ServerOptions) {
    this.routing = new LiveRouting(options.validateMessages === true);
    requireThat(
      (options.trustedProxyAddresses ?? []).every((value) => isIP(value) !== 0),
      "invalid_arguments",
      "Trusted proxies must be exact IP addresses.",
    );
    requireThat(
      new Set(options.packagePublisherPrincipalIds ?? []).size ===
        (options.packagePublisherPrincipalIds ?? []).length &&
        (options.packagePublisherPrincipalIds ?? []).every((principalId) =>
          options.credentials.some(
            (value) => value.principalId === principalId,
          ),
        ),
      "invalid_arguments",
      "Package publishers must be distinct configured Hive principals.",
    );
    this.base = new URL(options.publicBaseUrl);
    requireThat(
      ["http:", "https:"].includes(this.base.protocol) &&
        !this.base.username &&
        !this.base.password &&
        !this.base.search &&
        !this.base.hash,
      "invalid_arguments",
      "Invalid canonical publicBaseUrl.",
    );
    requireThat(
      this.base.protocol === "https:" ||
        ["localhost", "127.0.0.1", "[::1]"].includes(this.base.hostname),
      "invalid_arguments",
      "Public browser authentication requires HTTPS.",
    );
    this.basePath = this.base.pathname.replace(/\/$/, "");
    this.uiFiles = new UiFiles(options.uiRoot ?? join(dirname(options.filename), "ui-releases"));
    const workerOptions = { ...options, uiRoot: this.uiFiles.root };
    delete workerOptions.observeMcpCall;
    delete workerOptions.sendBrowserPush;
    this.worker = new WorkerClient(workerOptions);
    this.browserPush = new BrowserPush(this.worker, this.base.href, options.sendBrowserPush);
    this.oauth = new HiveOAuth(this.base, options.credentials, {
      create: (state, activeCredentialReferences) =>
        this.worker.request({
          action: "oauth.family.create",
          state,
          activeCredentialReferences,
        }),
      rotate: (rotation) =>
        this.worker.request({ action: "oauth.family.rotate", rotation }),
    });
    this.packages = options.packageRoot
      ? new PackageRegistry(
          options.packageRoot,
          this.basePath,
          {},
          (input) => this.installUiPackage(input),
          this.worker,
        )
      : null;
    this.worker.onRoutingUpdate = (value) => this.routing.update(value);
    this.worker.onEventsAvailable = (throughSequence) =>
      this.eventsAvailable(throughSequence);
    this.worker.onChanges = (scopes) => {
      for (const peer of this.peers) {
        const selected = scopes.filter(scope => peer.changeScopes.some(watched =>
          scope === watched || scope.startsWith(watched + "/") || watched.startsWith(scope + "/")));
        if (selected.length) this.send(peer, { jsonrpc: "2.0", method: "notifications.changed", params: { scopes: selected } });
      }
    };
    this.worker.onFailure = () => {
      this.routing.clear();
      this.authenticatedSockets = new WeakMap();
      for (const peer of this.peers) peer.socket.terminate();
      this.rejectAll(
        new IvyError(
          "outcome_unknown",
          "Hive storage restarted; reconcile the original operation.",
          "unknown",
        ),
      );
      this.scheduleRecovery();
    };
    this.server = createServer(
      {
        maxHeaderSize: 16 * 1024,
        requestTimeout: 5 * 60_000,
        headersTimeout: 10_000,
        connectionsCheckingInterval: 1000,
      },
      (request, response) => {
        void this.http(request, response);
      },
    );
    this.server.maxConnections = 1024;
    this.server.on("upgrade", (request, socket, head) => {
      void this.upgrade(request, socket as Socket, head);
    });
    this.server.on("clientError", (_error, socket) => {
      if (socket.writable)
        socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
    });
    this.server.on("error", () => {
      /* listen failures are returned by start; active errors expose no request contents */
    });
  }
  private rpcClient(principalId: string): RpcClient {
    const context: ConnectionContext = {
      principalId,
      credentialDigest: digest("package-publisher:" + principalId),
      transport: "http",
    };
    return {
      request: async (method, params) => {
        const result = await this.worker.request<KernelResult>({
          action: "execute",
          context,
          request: { jsonrpc: "2.0", id: randomUUID(), method, params },
        });
        return result.value as never;
      },
    };
  }
  private async installUiPackage(input: {
    manifest: Host.ReleaseManifest;
    artifactRoot: string;
    publisherPrincipalId: string;
    buildId: string;
  }): Promise<void> {
    const app = input.manifest.app;
    requireThat(
      input.manifest.kind === "app" &&
        app &&
        app.appId === input.manifest.componentId,
      "build_mismatch",
      "App package manifest does not identify its app.",
    );
    const directory = resolve(input.artifactRoot, app.dist);
    const definition = JSON.parse(
      await readFile(join(directory, "ivy-ui.json"), "utf8"),
    ) as Operation.UiDefinition;
    requireThat(
      definition.metadata.uiId === app.appId &&
        definition.entryPath === app.entryPath,
      "build_mismatch",
      "UI bundle definition differs from its package manifest.",
    );
    const client = this.rpcClient(input.publisherPrincipalId),
      checkedBundle = await readUiBundle(directory, definition);
    const pointers = await this.worker.request<{ current_release_id: string | null }[]>({ action: "ui.pointers", uiId: app.appId });
    const expectedReleaseId = pointers[0]?.current_release_id ?? null;
    const assets = checkedBundle.assets;
    await this.uiFiles.install(directory, app.appId, checkedBundle.releaseId, assets);
    if (expectedReleaseId === checkedBundle.releaseId) return;
    const mutationId = await newOperationId(client, "package-" + input.buildId.slice(7, 47));
    for (const contract of definition.dataContracts) {
      requireThat(contract.owner.kind === "agent", "contract_owner_mismatch",
        "UI publication can only register agent-owned data contracts.");
      await client.request("contracts.register", {
        mutationId: deriveOperationId(mutationId, ["contract", contract.key, contract.version]),
        definition: contract,
      });
    }
    await client.request("uis.deploy", {
      mutationId,
      expectedReleaseId,
      metadata: definition.metadata,
      release: {
        releaseId: checkedBundle.releaseId,
        entryPath: definition.entryPath,
        requirements: definition.requirements,
        assets,
      },
    });
  }
  async start(): Promise<{ port: number; host: string }> {
    try {
      await this.worker.start();
      this.oauth.initialize(
        await this.worker.request<{ signingKey: string; referenceKey: string }>(
          { action: "oauth.initialize" },
        ),
      );
      await this.packages?.initialize();
      await new Promise<void>((resolve, reject) => {
        this.server.once("error", reject);
        this.server.listen(
          this.options.listenPort ?? 39081,
          this.options.listenHost ?? "127.0.0.1",
          () => {
            this.server.off("error", reject);
            resolve();
          },
        );
      });
    } catch (error) {
      this.stopped = true;
      if (this.recoveryTimer) clearTimeout(this.recoveryTimer);
      await this.worker.close();
      throw error;
    }
    this.browserPush.start();
    this.healthTimer = setInterval(() => {
      for (const peer of this.peers) {
        if (!peer.alive) {
          peer.socket.terminate();
          continue;
        }
        peer.alive = false;
        peer.socket.ping();
        // Browser JavaScript cannot observe protocol ping/pong frames.
        if (peer.changeScopes.length) this.send(peer, { jsonrpc: "2.0", method: "notifications.changed", params: { scopes: [] } });
      }
    }, 10_000);
    const address = this.server.address();
    requireThat(
      address && typeof address !== "string",
      "internal_error",
      "Hive listener address is unavailable.",
    );
    return { port: address.port, host: address.address };
  }
  async collectPackageArtifacts(): Promise<void> {
    await this.packages?.collectArtifacts();
  }
  private scheduleRecovery(): void {
    if (this.stopped || this.recoveryTimer) return;
    this.recoveryTimer = setTimeout(() => {
      this.recoveryTimer = null;
      void this.worker.start().catch(() => this.scheduleRecovery());
    }, 1000);
  }
  private eventsAvailable(throughSequence: number): void {
    this.browserPush.wake();
    if (
      !Number.isSafeInteger(throughSequence) ||
      throughSequence < 1 ||
      this.stopped
    )
      return;
    this.eventsAvailableThroughSequence = Math.max(
      this.eventsAvailableThroughSequence,
      throughSequence,
    );
    if (this.eventsAvailabilityScheduled) return;
    this.eventsAvailabilityScheduled = true;
    setImmediate(() => {
      this.eventsAvailabilityScheduled = false;
      const available = this.eventsAvailableThroughSequence;
      this.eventsAvailableThroughSequence = 0;
      if (!available || this.stopped) return;
      const notification = {
        jsonrpc: "2.0",
        method: "events.available",
        params: { throughSequence: available },
      };
      const encoded = encodeJson(notification);
      for (const peer of this.peers)
        if (peer.context.serviceNodeId && peer.context.generation)
          this.send(peer, notification, encoded);
    });
  }
  async close(): Promise<void> {
    this.stopped = true;
    await this.browserPush.close();
    if (this.healthTimer) clearInterval(this.healthTimer);
    if (this.recoveryTimer) clearTimeout(this.recoveryTimer);
    this.rejectAll(
      new IvyError(
        "outcome_unknown",
        "Hive is shutting down; reconcile the original operation.",
        "unknown",
      ),
    );
    for (const peer of this.peers) peer.socket.terminate();
    this.ws.close();
    this.server.closeAllConnections();
    await new Promise<void>((resolve) => {
      this.server.close(() => resolve());
    });
    await this.worker.close();
  }
  async setCredentials(
    credentials: KernelOptions["credentials"],
  ): Promise<void> {
    await this.withDispatchGate(async () => {
      await this.worker.request({
        action: "credentials.configure",
        credentials,
      });
      this.authenticatedSockets = new WeakMap();
      this.options.credentials = credentials;
      this.oauth.setCredentials(credentials);
      this.routing.retireUnconfiguredPrincipals(
        credentials.map((credential) => credential.principalId),
      );
      const valid = new Set(credentials.map((credential) => credential.digest));
      for (const peer of this.peers)
        if (!valid.has(peer.context.credentialDigest)) {
          if (peer.context.serviceNodeId && peer.context.generation)
            this.routing.disconnect(
              peer.context.serviceNodeId,
              peer.context.generation,
            );
          peer.socket.terminate();
        }
    });
  }
  private withDispatchGate<T>(work: () => Promise<T>): Promise<T> {
    return this.dispatchGate.run(work);
  }
  private headers(response: ServerResponse): void {
    // Brave's ChatGPT OAuth popup can expose this approval document with an
    // opaque origin. In that context Chromium rejects even the exact Ivy host
    // source for a native form submission. This page is scriptless and its form
    // action is fixed to Ivy; the POST handler still enforces the exact origin.
    response.setHeader(
      "Content-Security-Policy",
      uiContentPolicy(),
    );
    response.setHeader("X-Content-Type-Options", "nosniff");
    response.setHeader("X-Frame-Options", "DENY");
    // no-referrer makes navigation POSTs send Origin: null in real browsers, breaking the exact
    // same-origin login/logout check. same-origin still withholds referrers from external sites.
    response.setHeader("Referrer-Policy", "same-origin");
    response.setHeader("Cache-Control", "no-store");
  }
  private json(response: ServerResponse, status: number, value: unknown): void {
    if (response.writableEnded || response.destroyed) return;
    if (response.headersSent) {
      response.destroy();
      return;
    }
    response.writeHead(status, {
      "Content-Type": "application/json; charset=utf-8",
    });
    response.end(JSON.stringify(value));
  }
  private sameOrigin(request: IncomingMessage): void {
    requireThat(
      request.headers.origin === this.base.origin,
      "unauthenticated",
      "Browser action requires the exact Hive origin.",
    );
  }
  private mcpOrigin(request: IncomingMessage): void {
    const origin = request.headers.origin;
    if (origin === undefined) return;
    let parsed: URL | null = null;
    try {
      if (typeof origin === "string" && origin !== "null")
        parsed = new URL(origin);
    } catch {
      /* rejected below */
    }
    requireThat(
      parsed?.origin === this.base.origin &&
        parsed.href === this.base.origin + "/",
      "forbidden",
      "MCP Origin is not allowed.",
    );
  }
  private clientAddress(request: IncomingMessage): string {
    const normalize = (value: string) =>
      value.startsWith("::ffff:") && isIP(value.slice(7)) === 4
        ? value.slice(7)
        : value;
    const peer = normalize(request.socket.remoteAddress ?? "local");
    if (
      !(this.options.trustedProxyAddresses ?? []).some(
        (value) => normalize(value) === peer,
      )
    )
      return peer;
    const forwarded = request.headers["x-ivy-client-ip"];
    if (forwarded === undefined) return peer; // Direct local maintenance retains the real socket peer.
    requireThat(
      typeof forwarded === "string" && isIP(forwarded) !== 0,
      "unauthenticated",
      "The trusted proxy must overwrite X-Ivy-Client-IP with the original client address.",
    );
    return normalize(forwarded);
  }
  private async authenticateBounded(
    request: IncomingMessage,
    transport: ConnectionContext["transport"],
    mcpResource?: string,
  ): Promise<ConnectionContext> {
    if (
      request.headers.authorization === undefined &&
      (request.method !== "GET" || transport === "ws")
    )
      this.sameOrigin(request);
    let cache = this.authenticatedSockets.get(request.socket);
    if (!cache) {
      cache = new Map();
      this.authenticatedSockets.set(request.socket, cache);
      const contexts = cache;
      request.socket.once("close", () => contexts.clear());
    }
    // Proxies can reuse one upstream socket for different users. Bind each exact
    // credential/cookie identity separately; never inherit another request's caller.
    const identity = digest(
      JSON.stringify([
        request.headers.authorization ?? null,
        request.headers.cookie ?? null,
        mcpResource ?? null,
      ]),
    );
    const existing = cache.get(identity);
    if (existing) {
      const context = { ...(await existing), transport };
      this.requireFresh(context);
      return context;
    }
    requireThat(
      cache.size < 16,
      "limit_exceeded",
      "Connection identity capacity reached.",
    );
    const key = this.clientAddress(request);
    this.arrivals.admit(key);
    const lease = this.authentication.acquire(key);
    const pending = this.authenticate(request, transport, mcpResource).finally(() =>
      lease.release(),
    );
    cache.set(identity, pending);
    try {
      const context = await pending;
      this.requireFresh(context);
      return context;
    } catch (error) {
      cache.delete(identity);
      throw error;
    }
  }
  private requireFresh(context: ConnectionContext): void {
    requireThat(
      context.expiresAt === undefined || context.expiresAt > Date.now(),
      "unauthenticated",
      "OAuth access token has expired.",
    );
  }
  private async authenticate(
    request: IncomingMessage,
    transport: ConnectionContext["transport"],
    mcpResource?: string,
  ): Promise<ConnectionContext> {
    const authorization = request.headers.authorization;
    if (authorization !== undefined) {
      requireThat(
        /^Bearer [^\s]+$/.test(authorization),
        "unauthenticated",
        "Expected a bearer credential.",
      );
      const oauth = this.oauth.authenticate(authorization.slice(7), mcpResource);
      if (oauth) return { ...oauth, transport };
      const context = {
        credentialDigest: digest(authorization.slice(7)),
        transport,
      };
      const principal = await this.worker.request<{ principalId: string }>({
        action: "authenticate",
        context,
      });
      return { ...context, principalId: principal.principalId };
    }
    const token = this.sessionToken(request);
    requireThat(token !== null, "unauthenticated", "Sign in to Hive.");
    const sessionDigest = digest(token);
    const context = await this.worker.request<AuthenticatedContext>({
      action: "session.resolve",
      sessionDigest,
    });
    if (request.method !== "GET" || transport === "ws")
      this.sameOrigin(request);
    return { ...context, transport };
  }
  private async body(
    request: IncomingMessage,
    limit = MAX_FRAME,
  ): Promise<Buffer> {
    const declared = request.headers["content-length"];
    if (declared !== undefined)
      requireThat(
        /^\d+$/.test(declared) && Number(declared) <= limit,
        "frame_too_large",
        "Request body exceeds its limit.",
      );
    const chunks: Buffer[] = [];
    let size = 0;
    try {
      for await (const data of request) {
        const chunk = Buffer.isBuffer(data)
          ? data
          : Buffer.from(data as Uint8Array);
        this.bodies.get(request)?.grow(chunk.length);
        size += chunk.length;
        this.bufferedBodies += chunk.length;
        requireThat(
          size <= limit,
          "frame_too_large",
          "Request body exceeds its limit.",
        );
        requireThat(
          this.bufferedBodies <= 64 * 1024 * 1024,
          "limit_exceeded",
          "Hive request buffers are full.",
        );
        chunks.push(chunk);
      }
      return Buffer.concat(chunks, size);
    } finally {
      this.bufferedBodies -= size;
    }
  }
  private parse(bytes: Buffer): unknown {
    try {
      return JSON.parse(
        new TextDecoder("utf-8", { fatal: true }).decode(bytes),
      ) as unknown;
    } catch {
      return fail("invalid_frame", "Malformed JSON input.");
    }
  }
  private redirect(response: ServerResponse, path: string): void {
    response.writeHead(303, { Location: path });
    response.end();
  }
  private returnPath(input: string | null): string {
    if (!input) return this.basePath + "/";
    requireThat(
      input.startsWith("/") && !input.startsWith("//") && !input.includes("\\"),
      "invalid_arguments",
      "Login return path must stay inside Hive.",
    );
    const destination = new URL(input, this.base.origin);
    requireThat(
      destination.origin === this.base.origin &&
        (destination.pathname === this.basePath ||
          destination.pathname.startsWith(this.basePath + "/")),
      "invalid_arguments",
      "Login return path must stay inside Hive.",
    );
    return destination.pathname + destination.search + destination.hash;
  }
  private sessionToken(request: IncomingMessage): string | null {
    const matches = (request.headers.cookie ?? "")
      .split(";")
      .map((part) => part.trim())
      .filter((part) => part.startsWith("ivy_session="));
    return matches.length === 1
      ? matches[0]!.slice("ivy_session=".length)
      : null;
  }
  private cookie(value: string, clear = false): string {
    return `ivy_session=${value}; Path=${this.basePath || "/"}; HttpOnly; Secure; SameSite=Lax; Max-Age=${clear ? 0 : 365 * 24 * 60 * 60}`;
  }
  private async http(
    request: IncomingMessage,
    response: ServerResponse,
  ): Promise<void> {
    this.headers(response);
    const active = this.httpActive.get(request.socket) ?? 0;
    this.httpActive.set(request.socket, active + 1);
    let rpcId: string | number | null = null;
    let routePath = "";
    let lease: RequestLease | undefined;
    try {
      lease = this.requests.acquire("http:" + this.clientAddress(request));
      this.bodies.set(request, lease);
      requireThat(
        active < connectionInFlightRequests,
        "limit_exceeded",
        "Too many requests on this connection.",
      );
      const url = new URL(request.url ?? "/", this.base.origin);
      if (
        request.method === "GET" &&
        url.pathname === "/.well-known/oauth-authorization-server"
      ) {
        this.json(
          response,
          200,
          this.oauth.authorizationServerMetadata(this.basePath),
        );
        return;
      }
      requireThat(
        url.pathname === this.basePath ||
          url.pathname.startsWith(this.basePath + "/"),
        "not_found",
        "Path is outside this Hive installation.",
      );
      const path = url.pathname.slice(this.basePath.length) || "/";
      routePath = path;
      if (path === "/api/v1/dashboards/image.png") {
        requireThat(!this.options.resetBootstrapCredentialDigest, "maintenance_active", "Hive is under maintenance.");
        await dashboardImageHttp(request, response, url, async (node, args) => {
          const context: ConnectionContext = {
            transport: "http", principalId: "hive.dashboard-image", credentialDigest: "dashboard-image",
          };
          return this.forward(context, this.routing.prepareDashboardImage(context, node, args));
        });
        return;
      }
      if (request.method === "GET" && path === "/app/manifest.webmanifest") {
        response.writeHead(200, { "Content-Type": "application/manifest+json" });
        response.end(JSON.stringify(browserAppManifest(this.basePath)));
        return;
      }
      if (request.method === "GET" && path === "/app-worker.js") {
        response.writeHead(200, { "Content-Type": "text/javascript; charset=utf-8", "Service-Worker-Allowed": this.basePath + "/" });
        response.end(browserAppWorker);
        return;
      }
      if (request.method === "GET" && /^\/app\/icon-(180|192|512)\.png$/.test(path)) {
        requireThat(this.options.consoleRoot, "not_found", "App icons are not installed.");
        const bytes = await readFile(join(this.options.consoleRoot, "icons", path.slice("/app/".length)));
        response.writeHead(200, { "Content-Type": "image/png", "Cache-Control": "public, max-age=86400" });
        response.end(bytes);
        return;
      }
      const iconSurface =
        path === "/mcp-icon.svg"
          ? "ivy"
          : path === "/mcp-dev-icon.svg"
            ? "ivy_dev"
            : null;
      if (request.method === "GET" && iconSurface) {
        response.setHeader("Cache-Control", "public, max-age=86400");
        response.writeHead(200, {
          "Content-Type": "image/svg+xml; charset=utf-8",
        });
        response.end(mcpIconSvg(iconSurface));
        return;
      }
      if (
        request.method === "GET" &&
        (path === "/.well-known/oauth-protected-resource" ||
          path === "/.well-known/oauth-protected-resource/mcp-dev")
      ) {
        this.json(response, 200, this.oauth.protectedResourceMetadata(
          path.endsWith("/mcp-dev") ? this.oauth.developmentResource : this.oauth.resource,
        ));
        return;
      }
      if (
        request.method === "GET" &&
        path === "/.well-known/oauth-authorization-server"
      ) {
        this.json(
          response,
          200,
          this.oauth.authorizationServerMetadata(this.basePath),
        );
        return;
      }
      if (request.method === "POST" && path === "/oauth/register") {
        const key = this.clientAddress(request);
        this.arrivals.admit(key);
        const admission = this.authentication.acquire(key);
        try {
          requireThat(
            request.headers["content-type"]?.split(";")[0] ===
              "application/json",
            "invalid_arguments",
            "OAuth registration requires JSON.",
          );
          this.json(
            response,
            201,
            this.oauth.register(
              this.parse(await this.body(request, 32 * 1024)),
            ),
          );
        } finally {
          admission.release();
        }
        return;
      }
      if (request.method === "GET" && path === "/oauth/authorize") {
        const key = this.clientAddress(request);
        this.arrivals.admit(key);
        const admission = this.authentication.acquire(key);
        try {
          const pending = this.oauth.begin(url);
          response.writeHead(200, {
            "Content-Type": "text/html; charset=utf-8",
          });
          response.end(
            oauthPage(
              this.basePath,
              pending.transaction,
              pending.clientName,
              pending.scope,
            ),
          );
        } finally {
          admission.release();
        }
        return;
      }
      if (request.method === "POST" && path === "/oauth/authorize") {
        this.sameOrigin(request);
        const key = this.clientAddress(request);
        this.arrivals.admit(key);
        const admission = this.authentication.acquire(key);
        try {
          requireThat(
            request.headers["content-type"]?.split(";")[0] ===
              "application/x-www-form-urlencoded",
            "invalid_arguments",
            "OAuth authorization requires a form POST.",
          );
          const form = new URLSearchParams(
              (await this.body(request, 8192)).toString("utf8"),
            ),
            transaction = form.get("transaction") ?? "";
          const credential = form.get("credential") ?? "",
            pending = this.oauth.transaction(transaction);
          try {
            this.redirect(
              response,
              this.oauth.authorize(transaction, digest(credential)),
            );
          } catch (error) {
            if (
              !(error instanceof IvyError) ||
              error.code !== "unauthenticated"
            )
              throw error;
            response.writeHead(401, {
              "Content-Type": "text/html; charset=utf-8",
            });
            response.end(
              oauthPage(
                this.basePath,
                transaction,
                pending.clientName,
                pending.scope,
                true,
              ),
            );
          }
        } finally {
          admission.release();
        }
        return;
      }
      if (request.method === "POST" && path === "/oauth/token") {
        const key = this.clientAddress(request);
        this.arrivals.admit(key);
        const admission = this.authentication.acquire(key);
        try {
          requireThat(
            request.headers["content-type"]?.split(";")[0] ===
              "application/x-www-form-urlencoded",
            "invalid_arguments",
            "OAuth token exchange requires form encoding.",
          );
          this.json(
            response,
            200,
            await this.oauth.token(
              new URLSearchParams(
                (await this.body(request, 32 * 1024)).toString("utf8"),
              ),
            ),
          );
        } finally {
          admission.release();
        }
        return;
      }
      if (request.method === "GET" && path === "/health/live") {
        this.json(response, 200, { live: true });
        return;
      }
      if (request.method === "GET" && path === "/health/ready") {
        const status = await this.worker
          .request<{ ready: boolean }>({ action: "health" }, 1000, false)
          .catch(() => ({ ready: false }));
        const ready =
          status.ready && !this.options.resetBootstrapCredentialDigest;
        this.json(response, ready ? 200 : 503, { ready });
        return;
      }
      const uploadMatch =
        /^\/api\/v1\/packages\/uploads\/([0-9a-f-]{36})$/.exec(path);
      if (request.method === "PUT" && uploadMatch) {
        requireThat(
          this.packages,
          "service_unavailable",
          "Hive package storage is not configured.",
        );
        this.json(
          response,
          201,
          await this.packages.upload(request, uploadMatch[1]!),
        );
        return;
      }
      if (this.options.resetBootstrapCredentialDigest)
        requireThat(
          request.method === "POST" && path === "/api/v1/rpc",
          "maintenance_active",
          "Hive public access remains held during runtime reset bootstrap.",
        );
      if (path === "/login/theme.js" && request.method === "GET") {
        response.writeHead(200, {
          "Content-Type": "text/javascript; charset=utf-8",
        });
        response.end(loginThemeScript);
        return;
      }
      if (path === "/login" && request.method === "GET") {
        const target = this.returnPath(url.searchParams.get("returnTo"));
        response.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
        response.end(loginPage(this.basePath, target));
        return;
      }
      if (path === "/login" && request.method === "POST") {
        this.sameOrigin(request);
        requireThat(
          request.headers["content-type"]?.split(";")[0] ===
            "application/x-www-form-urlencoded",
          "invalid_arguments",
          "Login requires a form POST.",
        );
        const form = new URLSearchParams(
          (await this.body(request, 8192)).toString("utf8"),
        );
        const token = form.get("token");
        requireThat(
          token && token.length <= 4096,
          "unauthenticated",
          "A Hive credential is required.",
        );
        const target = this.returnPath(form.get("returnTo"));
        const key = this.clientAddress(request);
        this.arrivals.admit(key);
        const admission = this.authentication.acquire(key);
        let session: { sessionToken: string };
        try {
          session = await this.worker.request({
            action: "session.create",
            credentialDigest: digest(token),
          });
        } catch (error) {
          if (!(error instanceof IvyError) || error.code !== "unauthenticated")
            throw error;
          response.writeHead(401, {
            "Content-Type": "text/html; charset=utf-8",
          });
          response.end(loginPage(this.basePath, target, true));
          return;
        } finally {
          admission.release();
        }
        response.setHeader("Set-Cookie", this.cookie(session.sessionToken));
        this.redirect(response, target);
        return;
      }
      let context: ConnectionContext;
      const mcpSurface = path === "/mcp" ? "ivy" : path === "/mcp-dev" ? "ivy_dev" : null;
      const slug = path.split('/')[1] ?? '';
      const shortUiId = request.method === 'GET' && /^[a-z][a-z0-9-]{0,63}$/.test(slug) && !reservedUiSlugs.includes(slug)
        ? await this.worker.request<string | null>({ action: 'ui.resolveSlug', slug }) : null;
      try {
        context = await this.authenticateBounded(
          request,
          mcpSurface ? "mcp" : "http",
          mcpSurface === "ivy_dev" ? this.oauth.developmentResource : mcpSurface ? this.oauth.resource : undefined,
        );
      } catch (error) {
        if (
          request.method === "GET" &&
          error instanceof IvyError &&
          error.code === "unauthenticated" &&
          (path === "/" ||
            ((shortUiId || path.startsWith("/ui/") || path.startsWith("/console/")) &&
              request.headers["sec-fetch-mode"] === "navigate" &&
              request.headers["sec-fetch-dest"] === "document"))
        ) {
          const target = this.returnPath(url.pathname + url.search);
          // The browser carries the original fragment to /login; its script adds it to returnTo.
          this.redirect(response, `${this.basePath}/login?returnTo=${encodeURIComponent(target)}&resume=1`);
          return;
        }
        throw error;
      }
      if (mcpSurface) this.mcpOrigin(request);
      if (path === "/api/browser-push" && request.method === "POST") {
        this.sameOrigin(request);
        const input = JSON.parse((await this.body(request, 8192)).toString("utf8")) as BrowserPushRequest;
        requireThat(input && typeof input === "object" && ["status", "subscribe", "unsubscribe", "test"].includes(input.action), "invalid_arguments", "Invalid browser push action.");
        const result = await this.worker.request({ action: "browserPush.request", context, request: input });
        this.json(response, 200, result);
        if (input.action === "test") this.browserPush.wake();
        return;
      }
      if (context.sessionDigest) {
        const token = this.sessionToken(request);
        if (token) response.setHeader("Set-Cookie", this.cookie(token));
      }
      if (path === "/logout" && request.method === "POST") {
        this.sameOrigin(request);
        await this.worker.request({ action: "session.end", context });
        this.authenticatedSockets = new WeakMap();
        for (const peer of this.peers)
          if (
            peer.context.sessionDigest &&
            peer.context.credentialDigest === context.credentialDigest
          )
            peer.socket.terminate();
        response.setHeader("Set-Cookie", this.cookie("", true));
        this.redirect(response, this.basePath + "/login");
        return;
      }
      if (
        (path === "/api/v1/rpc" || mcpSurface) &&
        request.method === "POST"
      ) {
        requireThat(
          request.headers["content-type"]?.split(";")[0] === "application/json",
          "invalid_arguments",
          "RPC requires JSON content type.",
        );
        const received = await this.body(request);
        const frame = this.parse(received);
        requireThat(
          !Array.isArray(frame) && frame !== null && typeof frame === "object",
          "invalid_frame",
          "Expected a single request envelope.",
        );
        rpcId = validRequestId((frame as { id?: unknown }).id);
        if (mcpSurface) {
          await handleMcp(
            request,
            response,
            frame,
            context,
            (context, request) =>
              this.invoke(context, request, received.length),
            {
              surface: mcpSurface,
              version: this.options.version,
              publicBaseUrl: this.options.publicBaseUrl,
              resourceMetadataUrl: `${this.base.origin}${this.basePath}/.well-known/oauth-protected-resource${mcpSurface === "ivy_dev" ? "/mcp-dev" : ""}`,
              catalog: (name) => this.worker.request({ action: 'mcp.catalog', ...(name ? { name } : {}) }),
              cursors: {
                create: async (identity, offset) =>
                  (
                    await this.worker.request<{ cursor: string }>({
                      action: "mcp.cursor.create",
                      identity,
                      offset,
                    })
                  ).cursor,
                read: async (identity, cursor) =>
                  (
                    await this.worker.request<{ offset: number }>({
                      action: "mcp.cursor.read",
                      identity,
                      cursor,
                    })
                  ).offset,
              },
              ...(this.options.observeMcpCall
                ? { observeCall: this.options.observeMcpCall }
                : {}),
            },
          );
          return;
        }
        const result = await this.invoke(context, frame, received.length);
        this.json(response, 200, { jsonrpc: "2.0", id: rpcId, result });
        return;
      }
      if (mcpSurface) {
        response.setHeader("Allow", "POST");
        this.json(response, 405, { error: "method_not_allowed" });
        return;
      }
      if (path === "/api/v1/packages/catalog" && request.method === "GET") {
        requireThat(
          this.packages,
          "service_unavailable",
          "Hive package storage is not configured.",
        );
        const after = Number(url.searchParams.get("after") ?? "0"),
          snapshot = await this.packages.catalogSnapshot(
            after,
            url.searchParams.get("include") === "all",
          ),
          catalog = snapshot.catalog;
        response.setHeader("X-Ivy-Object-Id", snapshot.objectId);
        response.setHeader(
          "X-Ivy-Object-Revision",
          String(snapshot.objectRevision),
        );
        response.setHeader("ETag", `"${catalog.revision}"`);
        if (after >= catalog.revision) {
          response.writeHead(204);
          response.end();
          return;
        }
        this.json(response, 200, catalog);
        return;
      }
      const artifactMatch =
        /^\/api\/v1\/packages\/([a-z][a-z0-9-]{0,127})\/([0-9]+\.[0-9]+\.[0-9]+)\/artifact$/.exec(
          path,
        );
      if (request.method === "GET" && artifactMatch) {
        requireThat(
          this.packages,
          "service_unavailable",
          "Hive package storage is not configured.",
        );
        const artifact = await this.packages.artifact(
          artifactMatch[1]!,
          artifactMatch[2]!,
        );
        response.writeHead(200, {
          "Content-Type": "application/gzip",
          "Content-Length": artifact.entry.bytes,
          ETag: `"${artifact.entry.archiveHash}"`,
          "Cache-Control": "private, max-age=31536000, immutable",
        });
        await new Promise<void>((resolve, reject) => {
          const stream = createReadStream(artifact.path);
          stream.once("error", reject);
          response.once("finish", resolve);
          response.once("close", resolve);
          stream.pipe(response);
        });
        return;
      }
      if (request.method === "GET" && (shortUiId || path.startsWith("/ui/"))) {
        if (shortUiId && path === '/' + slug) {
          this.redirect(response, `${this.basePath}/${slug}/${url.search}`);
          return;
        }
        const parts = shortUiId ? [shortUiId, ...path.split('/').slice(2).map(part => decodeURIComponent(part))] : path
          .split("/")
          .slice(2)
          .map((part) => decodeURIComponent(part));
        const uiId = parts[0]!;
        const historical = parts[1] === "releases" && parts.length >= 4;
        const currentEntry = parts.length === 2 && parts[1] === "";
        const currentAsset = parts.length >= 2 && !!parts[1] && parts[1] !== "releases";
        if (
          historical &&
          parts.slice(3).join("/") === "index.html" &&
          request.headers["sec-fetch-mode"] === "navigate"
        ) {
          this.redirect(
            response,
            `${this.basePath}/${shortUiId ? slug : 'ui/' + encodeURIComponent(uiId)}/${url.search}`,
          );
          return;
        }
        requireThat(
          historical || currentEntry || currentAsset,
          "not_found",
          "UI asset path not found.",
        );
        const selected = await this.worker.request<{ releaseId: string; asset: Operation.UiAsset }>(
          historical
            ? {
                action: "asset",
                context,
                uiId,
                releaseId: parts[2]!,
                path: parts.slice(3).join("/"),
              }
            : {
                action: "ui.currentAsset",
                context,
                uiId,
                ...(parts[1] ? { path: parts.slice(1).join("/") } : {}),
              },
        );
        const bytes = await this.uiFiles.read(uiId, selected.releaseId, selected.asset);
        response.setHeader('Content-Security-Policy', uiContentPolicy(selected.asset.path));
        if (selected.asset.path.endsWith('.sandbox.html')) response.removeHeader('X-Frame-Options');
        const etag = '"' + selected.asset.contentHash + '"';
        response.setHeader("ETag", etag);
        response.setHeader("Vary", "Cookie, Authorization");
        response.setHeader(
          "Cache-Control",
          historical
            ? "private, max-age=31536000, immutable"
            : "private, no-cache",
        );
        if (request.headers["if-none-match"] === etag) {
          response.writeHead(304);
          response.end();
          return;
        }
        response.writeHead(200, {
          "Content-Type": selected.asset.mediaType,
          "Content-Length": bytes.length,
        });
        response.end(bytes);
        return;
      }
      if (
        request.method === "GET" &&
        (path === "/" || path.startsWith("/console/"))
      ) {
        requireThat(
          this.options.consoleRoot,
          "service_unavailable",
          "Hive Console artifact has not been built.",
        );
        if (
          (path === "/" && url.pathname !== this.basePath + "/") ||
          path === "/console/index.html"
        ) {
          this.redirect(response, this.basePath + "/");
          return;
        }
        const relative =
          path === "/"
            ? "index.html"
            : decodeURIComponent(path.slice("/console/".length));
        assetPath(relative);
        const root = await realpath(this.options.consoleRoot);
        const file = await realpath(resolve(join(root, relative))).catch(() =>
          fail("not_found", "Console asset not found."),
        );
        requireThat(
          file.startsWith(root + sep),
          "not_found",
          "Console asset not found.",
        );
        const details = await stat(file);
        requireThat(
          details.isFile() && details.size <= MAX_FRAME,
          "not_found",
          "Console asset not found.",
        );
        const mime = file.endsWith(".html")
          ? "text/html"
          : file.endsWith(".js")
            ? "text/javascript"
            : file.endsWith(".css")
              ? "text/css"
              : file.endsWith(".woff2")
                ? "font/woff2"
                : "application/octet-stream";
        response.writeHead(200, { "Content-Type": mime });
        response.end(await readFile(file));
        return;
      }
      fail("not_found", "Hive path or method not found.");
    } catch (error) {
      if (
        (routePath === "/mcp" || routePath === "/mcp-dev") &&
        error instanceof IvyError &&
        error.code === "unauthenticated"
      )
        response.setHeader(
          "WWW-Authenticate",
          `Bearer resource_metadata="${this.base.origin}${this.basePath}/.well-known/oauth-protected-resource${routePath === "/mcp-dev" ? "/mcp-dev" : ""}", scope="hive"`,
        );
      if (routePath === "/oauth/token" || routePath === "/oauth/register") {
        const failure = IvyError.from(error),
          capacity = failure.code === "limit_exceeded";
        this.json(
          response,
          failure.code === "unauthenticated" ? 401 : capacity ? 429 : 400,
          {
            error:
              failure.code === "unauthenticated"
                ? "invalid_grant"
                : capacity
                  ? "temporarily_unavailable"
                  : "invalid_request",
            error_description: failure.message,
          },
        );
        return;
      }
      const failure = rpcFailure(error, rpcId);
      this.json(response, failure.status, failure.body);
    } finally {
      this.bodies.delete(request);
      lease?.release();
      const remaining = (this.httpActive.get(request.socket) ?? 1) - 1;
      if (remaining) this.httpActive.set(request.socket, remaining);
      else this.httpActive.delete(request.socket);
    }
  }
  async invoke(
    context: ConnectionContext,
    request: unknown,
    receivedBytes = 0,
  ): Promise<unknown> {
    this.requireFresh(context);
    const requestBytes = receivedBytes > 0 ? receivedBytes : 4096;
    const lease = this.requests.acquire(
      context.credentialDigest,
      1024 + requestBytes,
    );
    try {
      if (this.options.resetBootstrapCredentialDigest)
        requireThat(
          context.transport === "http" &&
            context.credentialDigest ===
              this.options.resetBootstrapCredentialDigest,
          "maintenance_active",
          "Hive accepts only the configured ui bootstrap while the runtime reset is held.",
        );
      this.requireFresh(context);
      const direct = this.direct(context, request);
      if (direct) return await direct;
      return await this.withDispatchGate(async () => {
        this.requireFresh(context);
        const prepared = await this.worker.request<KernelResult>(
          { action: "execute", context, request },
          30_000,
          true,
          requestBytes,
        );
        return prepared.value;
      });
    } finally {
      lease.release();
    }
  }
  private checkedDirect(
    method: string,
    work: Promise<unknown>,
  ): Promise<unknown> {
    return work.then((value) => {
      if (this.options.validateMessages === true) validateOutput(method, value);
      return value;
    });
  }
  private direct(
    context: ConnectionContext,
    request: unknown,
    peer?: Peer,
  ): Promise<unknown> | null {
    this.requireFresh(context);
    const method = (request as { method?: string } | null)?.method;
    if (
      ![
        "tools.call",
        "discovery.call",
        "notifications.subscribe",
        "packages.catalog",
        "packages.authorizeUpload",
      ].includes(method ?? "")
    )
      return null;
    validateRequest(request);
    if (method === "notifications.subscribe") {
      validateInput(method, request.params);
      requireThat(
        peer && context.transport === "ws",
        "invalid_arguments",
        "Notification subscriptions require the current WebSocket session.",
      );
      const filters = structuredClone(
        (request.params as Operation.NotificationsSubscribeParams).filters,
      );
      peer.notificationFilters = filters;
      const changes = (request.params as Operation.NotificationsSubscribeParams).changes;
      peer.changeScopes = structuredClone(changes ?? []);
      return this.checkedDirect(
        method,
        Promise.resolve({ subscribed: filters.length, ...(changes ? { changes: changes.length } : {}) }),
      );
    }
    if (method === "packages.catalog") {
      validateInput(method, request.params);
      requireThat(
        this.packages,
        "service_unavailable",
        "Hive package storage is not configured.",
      );
      const params = request.params as Operation.PackagesCatalogParams;
      return this.checkedDirect(
        method,
        this.packages.catalogSnapshot(params.after, params.includeUis),
      );
    }
    if (method === "packages.authorizeUpload") {
      validateInput(method, request.params);
      requireThat(
        this.packages,
        "service_unavailable",
        "Hive package storage is not configured.",
      );
      requireThat(
        this.options.packagePublisherPrincipalIds?.includes(
          context.principalId,
        ),
        "forbidden",
        "Caller may not publish Ivy packages.",
      );
      return this.checkedDirect(
        method,
        this.packages.authorize(context.principalId, request.params),
      );
    }
    validateToolRouting(request.params, method === "discovery.call");
    if (method === "discovery.call") {
      const params = request.params as Operation.DiscoveryCallParams;
      if (params.serviceName === "hive") {
        requireThat(
          params.expectedCallerPrincipalId === undefined ||
            params.expectedCallerPrincipalId === context.principalId,
          "caller_changed",
          "Return to the original caller before reconciling or repeating this action.",
        );
        const resolved = resolveCoreCall(params);
        if (
          ![
            "notifications.subscribe",
            "packages.catalog",
            "packages.authorizeUpload",
          ].includes(resolved.method)
        )
          return null;
        return this.direct(
          context,
          { jsonrpc: "2.0", id: request.id, ...resolved },
          peer,
        );
      }
      requireThat(
        params.serviceNodeId,
        "invalid_arguments",
        "Service calls require the exact described serviceNodeId.",
      );
    }
    const call = this.routing.prepare(
      context,
      request.params as unknown as Parameters<LiveRouting["prepare"]>[1],
    );
    return this.forward(context, call);
  }
  private subscribed(
    peer: Peer,
    notification: Transport.ProviderNotification,
  ): boolean {
    return peer.notificationFilters.some(
      (filter) =>
        filter.namespace === notification.params.namespace &&
        filter.name === notification.params.name &&
        filter.version === notification.params.version &&
        (filter.serviceNodeId === undefined ||
          filter.serviceNodeId === notification.params.serviceNodeId),
    );
  }
  private async upgrade(
    request: IncomingMessage,
    socket: Socket,
    head: Buffer,
  ): Promise<void> {
    let lease: RequestLease | undefined;
    try {
      requireThat(
        !this.options.resetBootstrapCredentialDigest,
        "maintenance_active",
        "Hive WebSocket access remains held during runtime reset bootstrap.",
      );
      requireThat(
        new URL(request.url ?? "/", this.base.origin).pathname ===
          this.basePath + "/ws",
        "not_found",
        "WebSocket path not found.",
      );
      lease = this.connections.acquire(this.clientAddress(request));
      const held = lease;
      socket.once("close", () => held.release());
      const context = await this.authenticateBounded(request, "ws");
      this.ws.handleUpgrade(request, socket, head, (ws) =>
        this.connected(ws, context),
      );
    } catch {
      lease?.release();
      if (socket.writable)
        socket.end("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n");
    }
  }
  private send(peer: Peer, value: unknown, encoded?: string): void {
    if (peer.socket.readyState !== WebSocket.OPEN) return;
    let text: string;
    try {
      text = encoded ?? encodeJson(value);
    } catch {
      const id = validRequestId((value as { id?: unknown } | null)?.id);
      if (id === null) {
        peer.socket.terminate();
        return;
      }
      text = encodeJson(
        rpcFailure(
          new IvyError(
            "content_too_large",
            "Response exceeds the transport byte limit.",
            "unknown",
          ),
          id,
        ).body,
      );
    }
    if (
      peer.socket.bufferedAmount + Buffer.byteLength(text) >
      managementSocketBufferBytes
    ) {
      peer.socket.terminate();
      return;
    }
    peer.socket.send(text, (error) => {
      if (error) peer.socket.terminate();
    });
  }
  private connected(socket: WebSocket, context: ConnectionContext): void {
    const peer: Peer = {
      socket,
      context,
      chain: new DispatchGate(connectionInFlightRequests),
      active: 0,
      alive: true,
      diagnosedAt: 0,
      notificationFilters: [],
      changeScopes: [],
    };
    const expiryTimer =
      context.expiresAt === undefined
        ? null
        : setTimeout(
            () => socket.terminate(),
            Math.max(0, context.expiresAt - Date.now()),
          );
    this.peers.add(peer);
    socket.on("pong", () => {
      peer.alive = true;
    });
    socket.on("error", () => undefined);
    socket.on("close", () => {
      if (expiryTimer) clearTimeout(expiryTimer);
      this.peers.delete(peer);
      if (
        peer.context.serviceNodeId &&
        peer.context.generation &&
        this.routing.disconnect(
          peer.context.serviceNodeId,
          peer.context.generation,
        )
      )
        void this.worker
          .request({
            action: "disconnect",
            serviceNodeId: peer.context.serviceNodeId,
          })
          .catch(() => undefined);
      for (const [id, call] of this.forwarded)
        if (call.peer === peer) {
          clearTimeout(call.timer);
          this.forwarded.delete(id);
          call.reject(
            new IvyError(
              "outcome_unknown",
              "Provider connection was lost after dispatch.",
              "unknown",
            ),
          );
        }
    });
    socket.on("message", (data, binary) => {
      let frame: unknown,
        byteLength = 0;
      try {
        requireThat(
          !binary,
          "invalid_frame",
          "Hive WebSocket frames must be JSON text.",
        );
        const bytes = Buffer.isBuffer(data)
          ? data
          : Buffer.from(data as ArrayBuffer);
        byteLength = bytes.length;
        frame = this.parse(bytes);
      } catch (error) {
        this.send(peer, rpcFailure(error, null).body);
        return;
      }
      const object =
        frame !== null && typeof frame === "object" && !Array.isArray(frame)
          ? (frame as Record<string, unknown>)
          : null;
      if (
        object &&
        ("result" in object || "error" in object) &&
        typeof object["id"] === "string"
      ) {
        void this.providerResult(peer, object);
        return;
      }
      if (object && !("id" in object)) {
        if (object["method"] === "service.notification") {
          try {
            validateServiceNotificationFrame(frame);
            const params = frame.params;
            this.routing.notification(peer.context, params);
            const notification: Transport.ProviderNotification = {
              jsonrpc: "2.0",
              method: "notifications.provider",
              params: {
                ...params,
                serviceNodeId: peer.context.serviceNodeId!,
                generation: peer.context.generation!,
              },
            };
            const encoded = encodeJson(notification, 1024 * 1024);
            for (const client of this.peers)
              if (client !== peer && this.subscribed(client, notification))
                this.send(client, notification, encoded);
          } catch {
            this.protocolDiagnostic(
              peer,
              "invalid_frame",
              "Transient provider frame was rejected.",
            );
          }
        }
        return;
      }
      try {
        this.requireFresh(peer.context);
      } catch (error) {
        this.send(peer, rpcFailure(error, validRequestId(object?.["id"])).body);
        peer.socket.terminate();
        return;
      }
      if (peer.active >= connectionInFlightRequests) {
        this.send(
          peer,
          rpcFailure(
            new IvyError("limit_exceeded", "Too many in-flight calls."),
            validRequestId(object?.["id"]),
          ).body,
        );
        return;
      }
      let lease: RequestLease;
      try {
        lease = this.requests.acquire(
          peer.context.credentialDigest,
          1024 + byteLength,
        );
      } catch (error) {
        this.send(peer, rpcFailure(error, validRequestId(object?.["id"])).body);
        return;
      }
      peer.active++;
      try {
        const direct = this.direct(peer.context, frame, peer);
        if (direct) {
          const id = validRequestId(object?.["id"]);
          void direct
            .then(
              (result) => this.send(peer, { jsonrpc: "2.0", id, result }),
              (error) => this.send(peer, rpcFailure(error, id).body),
            )
            .finally(() => {
              peer.active--;
              lease.release();
            });
          return;
        }
        if (peer.context.serviceNodeId) this.routing.caller(peer.context);
        if (
          object?.["method"] === "registry.sync" &&
          peer.context.serviceNodeId &&
          peer.context.generation
        )
          this.routing.suspend(
            peer.context.serviceNodeId,
            peer.context.generation,
          );
      } catch (error) {
        this.send(peer, rpcFailure(error, validRequestId(object?.["id"])).body);
        peer.active--;
        lease.release();
        return;
      }
      void peer.chain
        .run(async () => {
          this.requireFresh(peer.context);
          const id = validRequestId(object?.["id"]);
          try {
            const response = await this.withDispatchGate(async () => {
              this.requireFresh(peer.context);
              const connecting = object?.["method"] === "service.connect";
              // A replacement connection may have overtaken this queued frame.
              if (!connecting && peer.context.serviceNodeId)
                this.routing.caller(peer.context);
              if (connecting)
                this.connectionGeneration =
                  this.connectionGeneration === Number.MAX_SAFE_INTEGER
                    ? 1
                    : this.connectionGeneration + 1;
              const context = connecting
                ? { ...peer.context, generation: this.connectionGeneration }
                : peer.context;
              const prepared = await this.worker.request<KernelResult>(
                { action: "execute", context, request: frame },
                30_000,
                true,
                byteLength,
              );
              if (
                object?.["method"] === "service.connect" &&
                prepared.kind === "result"
              ) {
                const node = prepared.value as {
                  serviceNodeId: string;
                  generation: number;
                };
                peer.context = { ...peer.context, ...node };
                for (const old of this.peers)
                  if (
                    old !== peer &&
                    old.context.serviceNodeId === node.serviceNodeId
                  )
                    old.socket.terminate();
              }
              return { value: prepared.value };
            });
            this.send(peer, { jsonrpc: "2.0", id, result: response.value });
            peer.active--;
            lease.release();
          } catch (error) {
            this.send(peer, rpcFailure(error, id).body);
            peer.active--;
            lease.release();
          }
        })
        .catch((error) => {
          this.send(
            peer,
            rpcFailure(error, validRequestId(object?.["id"])).body,
          );
          peer.active--;
          lease.release();
        });
    });
  }
  private async forward(
    context: ConnectionContext,
    call: PreparedCall,
  ): Promise<unknown> {
    this.requireFresh(context);
    const peer = [...this.peers].find(
      (peer) =>
        peer.context.serviceNodeId === call.serviceNodeId &&
        peer.context.generation === call.generation &&
        peer.socket.readyState === WebSocket.OPEN,
    );
    requireThat(
      peer,
      "service_unavailable",
      "No live socket for the selected provider generation.",
    );
    requireThat(
      this.forwarded.size < 512,
      "limit_exceeded",
      "Hive live call limit reached.",
    );
    requireThat(
      [...this.forwarded.values()].filter((pending) => pending.peer === peer)
        .length < 64,
      "limit_exceeded",
      "Provider connection call/buffer limit reached.",
    );
    const id = "hive-call-" + randomUUID();
    const request = {
      jsonrpc: "2.0",
      id,
      method: "provider.invoke",
      params: {
        qualifiedName: call.definition.namespace + "." + call.definition.name,
        arguments: call.arguments,
        definitionHash: call.definitionHash,
        generation: call.generation,
        callerPrincipalId: call.callerPrincipalId,
        ...(call.operationId ? { operationId: call.operationId } : {}),
      },
    };
    const encoded = encodeJson(request);
    requireThat(
      peer.socket.bufferedAmount + Buffer.byteLength(encoded) <=
        managementSocketBufferBytes,
      "limit_exceeded",
      "Provider connection call/buffer limit reached.",
    );
    const result = await new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.forwarded.delete(id);
        reject(
          new IvyError(
            "deadline_exceeded",
            "Provider response deadline exceeded; execution outcome is unknown.",
            "unknown",
          ),
        );
      }, this.options.callTimeoutMs ?? 30_000);
      this.forwarded.set(id, { peer, call, resolve, reject, timer });
      this.send(peer, request, encoded);
    });
    return this.routing.complete(call, result);
  }
  private async providerResult(
    peer: Peer,
    frame: Record<string, unknown>,
  ): Promise<void> {
    const id = String(frame["id"]),
      pending = this.forwarded.get(id);
    if (
      !pending ||
      pending.peer !== peer ||
      peer.context.generation !== pending.call.generation
    ) {
      this.protocolDiagnostic(
        peer,
        "stale_generation",
        "Unmatched provider response was ignored.",
      );
      return;
    }
    try {
      validateResponseFrame(frame);
      if ("error" in frame) {
        const error = frame["error"] as unknown as WireError;
        pending.reject(
          new IvyError(
            error.data.code,
            error.message,
            error.data.outcome,
            error.data.details,
          ),
        );
      } else pending.resolve(frame["result"]);
    } catch {
      pending.reject(
        new IvyError(
          "provider_contract_error",
          "Provider sent an invalid response envelope.",
          "unknown",
        ),
      );
    } finally {
      clearTimeout(pending.timer);
      this.forwarded.delete(id);
    }
  }
  private protocolDiagnostic(peer: Peer, code: string, message: string): void {
    if (Date.now() - peer.diagnosedAt < 5000) return;
    peer.diagnosedAt = Date.now();
    void this.worker
      .request({
        action: "protocol.diagnostic",
        context: peer.context,
        code,
        message,
      })
      .catch(() => undefined);
  }
  private rejectAll(error: IvyError): void {
    for (const pending of this.forwarded.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.forwarded.clear();
  }
}
