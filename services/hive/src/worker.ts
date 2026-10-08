import { parentPort, workerData } from "node:worker_threads";
import { createHmac, randomBytes } from "node:crypto";
import {
  canonical,
  digest,
} from "../../../packages/contracts/src/canonical.js";
import {
  IvyError,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import { HiveKernel } from "./kernel.js";
import { BrowserPushStore } from "./browser-push-store.js";
import type { KernelOptions } from "./kernel.js";
import type { OAuthRefreshFamily, WorkerAction } from "./worker-protocol.js";

if (!parentPort) throw new Error("Hive storage worker needs a parent port.");
let kernel: HiveKernel;
let browserPush: BrowserPushStore;
try {
  kernel = new HiveKernel(workerData as KernelOptions);
  browserPush = new BrowserPushStore(kernel.store, new URL(kernel.options.publicBaseUrl));
  parentPort.postMessage({ ready: true });
} catch (error) {
  parentPort.postMessage({ fatal: IvyError.from(error).toWire() });
  parentPort.close();
  throw error;
}
let announcedEventSequence = Number(
  kernel.store.get("SELECT MAX(sequence) AS sequence FROM events")?.[
    "sequence"
  ] ?? 0,
);
const oauthPrefix = "oauth_refresh:";
function oauthSecrets(): { signingKey: string; referenceKey: string } {
  const root = kernel.store.metadata("cursor_secret");
  requireThat(
    root && /^[a-f0-9]{64}$/.test(root),
    "storage_invalid",
    "Hive OAuth root key is unavailable.",
  );
  const key = Buffer.from(root, "hex"),
    derive = (label: string) =>
      createHmac("sha256", key).update(label).digest("hex");
  return {
    signingKey: derive("ivy/oauth/signing-key/v2"),
    referenceKey: derive("ivy/oauth/credential-reference-key/v2"),
  };
}
function credentialReference(digestValue: string): string {
  const key = Buffer.from(oauthSecrets().referenceKey, "hex");
  return (
    "ivyr2." +
    createHmac("sha256", key)
      .update("ivy/oauth/credential-reference/v2\0" + digestValue)
      .digest("base64url")
  );
}
function oauthFamilies(): { key: string; state: OAuthRefreshFamily }[] {
  return kernel.store
    .all("SELECT key,value FROM metadata WHERE key LIKE 'oauth_refresh:%'")
    .map((row) => {
      let state: unknown;
      try {
        state = JSON.parse(String(row["value"]));
      } catch {
        throw new IvyError(
          "storage_invalid",
          "Retained OAuth family state is invalid.",
        );
      }
      requireThat(
        state !== null && typeof state === "object" && !Array.isArray(state),
        "storage_invalid",
        "Retained OAuth family state is invalid.",
      );
      return { key: String(row["key"]), state: state as OAuthRefreshFamily };
    });
}
function collectOAuthFamilies(now = Date.now(), active?: Set<string>): number {
  let retained = 0;
  for (const family of oauthFamilies()) {
    if (
      family.state.revoked ||
      family.state.expiresAt <= now ||
      (active && !active.has(family.state.credentialReference))
    )
      kernel.store.run("DELETE FROM metadata WHERE key=?", family.key);
    else retained++;
  }
  return retained;
}

async function handle(
  message: WorkerAction,
  progress?: () => void,
): Promise<unknown> {
  switch (message.action) {
    case "browserPush.request": return browserPush.request(message.context, message.request);
    case "browserPush.keys": return browserPush.keys();
    case "browserPush.next": return browserPush.next();
    case "browserPush.complete": return browserPush.complete(message.id, message.status);
    case "execute":
      requireThat(
        message.context.expiresAt === undefined ||
          message.context.expiresAt > Date.now(),
        "unauthenticated",
        "OAuth access token has expired.",
      );
      return kernel.execute(message.context, message.request);
    case "authenticate":
      return kernel.store.authenticate(message.context);
    case "credentials.configure": {
      kernel.store.configureCredentials(message.credentials);
      kernel.store.transaction(() => {
        collectOAuthFamilies(
          Date.now(),
          new Set(
            message.credentials.map((value) =>
              credentialReference(value.digest),
            ),
          ),
        );
      });
      return { configured: true };
    }
    case "oauth.initialize":
      return oauthSecrets();
    case "oauth.family.create": {
      const active = new Set(message.activeCredentialReferences),
        state = message.state;
      requireThat(
        active.has(state.credentialReference) &&
          state.generation === 1 &&
          !state.revoked &&
          state.expiresAt > Date.now(),
        "unauthenticated",
        "OAuth refresh family binding is invalid.",
      );
      kernel.store.transaction(() => {
        const retained = collectOAuthFamilies(Date.now(), active);
        requireThat(
          retained < 1024,
          "limit_exceeded",
          "OAuth refresh-family capacity is full.",
        );
        const key = oauthPrefix + state.familyDigest;
        requireThat(
          !kernel.store.metadata(key),
          "target_conflict",
          "OAuth refresh family identity already exists.",
        );
        kernel.store.setMetadata(key, canonical(state));
      });
      return { created: true };
    }
    case "oauth.family.rotate": {
      const rotation = message.rotation,
        key = oauthPrefix + rotation.familyDigest,
        now = Date.now();
      const rotated = kernel.store.transaction(() => {
        const encoded = kernel.store.metadata(key);
        if (!encoded) return false;
        const state = JSON.parse(encoded) as OAuthRefreshFamily;
        const binding =
          !state.revoked &&
          state.expiresAt > now &&
          state.credentialReference === rotation.credentialReference &&
          state.clientId === rotation.clientId &&
          state.resource === rotation.resource &&
          state.scope === rotation.scope;
        const current =
          binding &&
          state.generation === rotation.presentedGeneration &&
          state.currentTokenDigest === rotation.presentedTokenDigest;
        if (!current) {
          kernel.store.setMetadata(key, canonical({ ...state, revoked: true }));
          return false;
        }
        requireThat(
          rotation.nextGeneration === state.generation + 1 &&
            rotation.nextExpiresAt > now,
          "invalid_arguments",
          "OAuth refresh successor is invalid.",
        );
        kernel.store.setMetadata(
          key,
          canonical({
            ...state,
            generation: rotation.nextGeneration,
            currentTokenDigest: rotation.nextTokenDigest,
            expiresAt: rotation.nextExpiresAt,
          }),
        );
        return true;
      });
      requireThat(
        rotated,
        "unauthenticated",
        "OAuth refresh token was already consumed or revoked.",
      );
      return { rotated: true };
    }
    case "mcp.cursor.create":
      return { cursor: kernel.store.cursor(message.identity, message.offset) };
    case "mcp.catalog":
      return kernel.discovery.mcpCatalog(message.name);
    case "mcp.cursor.read": {
      const offset = kernel.store.readCursor(message.identity, message.cursor);
      requireThat(
        typeof offset === "number" &&
          Number.isSafeInteger(offset) &&
          offset >= 0,
        "invalid_cursor",
        "Invalid MCP discovery boundary.",
      );
      return { offset };
    }
    case "session.resolve": {
      const row = kernel.store.get(
        "SELECT credential_digest FROM browser_sessions WHERE digest=?",
        message.sessionDigest,
      );
      requireThat(row, "unauthenticated", "Browser session is not active.");
      const context = {
        credentialDigest: String(row["credential_digest"]),
        sessionDigest: message.sessionDigest,
      };
      const principal = kernel.store.authenticate(context);
      return { ...context, principalId: principal.principalId };
    }
    case "session.create": {
      const context = { credentialDigest: message.credentialDigest };
      kernel.store.authenticate(context);
      const token = randomBytes(32).toString("base64url");
      kernel.store.run(
        "INSERT INTO browser_sessions VALUES (?,?,?)",
        digest(token),
        context.credentialDigest,
        new Date().toISOString(),
      );
      return { sessionToken: token };
    }
    case "session.end": {
      requireThat(
        message.context.sessionDigest,
        "unauthenticated",
        "Logout requires the browser session.",
      );
      kernel.store.run(
        "DELETE FROM browser_sessions WHERE digest=?",
        message.context.sessionDigest,
      );
      return { credentialDigest: message.context.credentialDigest };
    }
    case "disconnect":
      kernel.registry.disconnect(message.serviceNodeId);
      return { disconnected: true };
    case "asset":
      return kernel.uis.asset(message.uiId, message.releaseId, message.path);
    case "ui.currentAsset":
      return kernel.uis.currentAsset(message.uiId, message.path);
    case "ui.resolveSlug":
      return kernel.uis.resolveSlug(message.slug);
    case "ui.entry":
      return kernel.uis.entry(message.uiId);
    case "ui.pointers":
      return message.uiId === undefined
        ? kernel.store.all("SELECT app_id,current_release_id,previous_release_id FROM apps ORDER BY app_id")
        : kernel.store.all("SELECT app_id,current_release_id,previous_release_id FROM apps WHERE app_id=?", message.uiId);
    case "package.catalog":
      return kernel.packageCatalog.list(message.after, message.includeUis);
    case "package.seed": {
      const principalId = kernel.options.credentials[0]?.principalId;
      requireThat(
        principalId,
        "service_unavailable",
        "Hive package catalog requires one configured principal.",
      );
      return kernel.packageCatalog.seed(message.catalog, principalId);
    }
    case "package.prune": {
      const principalId = kernel.options.credentials[0]?.principalId;
      requireThat(principalId, "service_unavailable", "Hive package catalog requires one configured principal.");
      return kernel.packageCatalog.prune(principalId);
    }
    case "package.publish":
      return kernel.packageCatalog.publish(
        message.input,
        message.publisherPrincipalId,
      );
    case "protocol.diagnostic": {
      const now = new Date().toISOString();
      // Rejected protocol frames are completed events, not persistent unhealthy state.
      kernel.registry.diagnostic({
        code: message.code,
        resource: { serviceNodeId: message.context.serviceNodeId ?? null },
        source: "hive",
        severity: "warning",
        status: "resolved",
        firstObservedAt: now,
        lastObservedAt: now,
        message: message.message,
      });
      return { recorded: true };
    }
    case "diagnostic":
      kernel.registry.diagnostic(message.value);
      return { recorded: true };
    case "health":
      return { ready: kernel.store.get("SELECT 1 AS ready")?.["ready"] === 1 };
    case "retention.collect": {
      kernel.store.invalidate("system/retention");
      try {
        const result = kernel.retention.collect();
        kernel.uis.collectFiles();
        kernel.store.transaction(() => {
          collectOAuthFamilies();
        });
        return result;
      } catch (error) {
        kernel.retention.recordFailure(error);
        throw error;
      }
    }
    case "backup":
      return kernel.store.backupTo(message.destination, progress);
    case "close":
      kernel.close();
      return { closed: true };
  }
}

let chain = Promise.resolve();
let backupTask: Promise<void> | null = null;
function publishRoutes(payload: WorkerAction): void {
  if (payload.action === "close") return;
  for (const id of kernel.registry.changedNodes) {
    try {
      const node = kernel.registry.node(id);
      const catalog =
        payload.action === "execute" &&
        (payload.request as { method?: string })?.method === "registry.sync" &&
        payload.context.serviceNodeId === id &&
        node.synced
          ? kernel.registry.liveCatalog(id)
          : null;
      const generation = kernel.registry.changedGenerations.get(id);
      parentPort!.postMessage({
        routing: {
          node,
          ...(generation === undefined ? {} : { generation }),
          ...(catalog ? { catalog } : {}),
        },
      });
    } catch {
      /* A failed transaction can have rolled back a newly created node. */
    }
  }
  kernel.registry.changedNodes.clear();
  kernel.registry.changedGenerations.clear();
}
function publishEventsAvailable(): void {
  if (kernel.store.changes.size) {
    parentPort!.postMessage({ changes: [...kernel.store.changes] });
    kernel.store.changes.clear();
  }
  const throughSequence = Number(
    kernel.store.get("SELECT MAX(sequence) AS sequence FROM events")?.[
      "sequence"
    ] ?? 0,
  );
  if (throughSequence <= announcedEventSequence) return;
  announcedEventSequence = throughSequence;
  parentPort!.postMessage({ eventsAvailable: { throughSequence } });
}
parentPort.on("message", (message: { id: number; payload: WorkerAction }) => {
  chain = chain.then(async () => {
    parentPort!.postMessage({ id: message.id, started: true });
    if (message.payload.action === "backup") {
      if (backupTask) {
        parentPort!.postMessage({
          id: message.id,
          error: new IvyError(
            "limit_exceeded",
            "A storage backup is already active.",
          ).toWire(),
        });
        return;
      }
      // Online backup uses this same database connection, in asynchronous batches. Neither
      // its snapshot nor streaming checksum may hold the exclusive request chain.
      let reportedAt = 0;
      backupTask = handle(message.payload, () => {
        const now = Date.now();
        if (now - reportedAt < 1000) return;
        reportedAt = now;
        parentPort!.postMessage({ id: message.id, progress: true });
      })
        .then(
          (value) => {
            parentPort!.postMessage({ id: message.id, value });
          },
          (error) => {
            parentPort!.postMessage({
              id: message.id,
              error: IvyError.from(error).toWire(),
            });
          },
        )
        .finally(() => {
          backupTask = null;
        });
      return;
    }
    if (message.payload.action === "close") await backupTask;
    try {
      const value = await handle(message.payload);
      publishRoutes(message.payload);
      publishEventsAvailable();
      parentPort!.postMessage({ id: message.id, value });
    } catch (error) {
      publishRoutes(message.payload);
      publishEventsAvailable();
      parentPort!.postMessage({
        id: message.id,
        error: IvyError.from(error).toWire(),
      });
    }
    if (message.payload.action === "close") parentPort!.close();
  });
});
