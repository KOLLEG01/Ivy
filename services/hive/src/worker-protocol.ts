import type { ConnectionContext } from "./kernel.js";
import type { AuthContext, AuthenticatedContext } from "./store.js";
import type { Wire } from "../../../packages/contracts/src/generated.js";
import type { PackageUploadAuthorization } from "../../../packages/host-runtime/src/package-archive.js";
import type { BrowserPushRequest } from "./browser-push-store.js";
export interface OAuthRefreshFamily {
  familyDigest: string;
  credentialReference: string;
  clientId: string;
  resource: string;
  scope: string;
  generation: number;
  currentTokenDigest: string;
  expiresAt: number;
  revoked: boolean;
}
export interface OAuthRefreshRotation {
  familyDigest: string;
  credentialReference: string;
  clientId: string;
  resource: string;
  scope: string;
  presentedGeneration: number;
  presentedTokenDigest: string;
  nextGeneration: number;
  nextTokenDigest: string;
  nextExpiresAt: number;
}
export type WorkerAction =
  | { action: "browserPush.request"; context: AuthenticatedContext; request: BrowserPushRequest }
  | { action: "browserPush.keys" }
  | { action: "browserPush.next" }
  | { action: "browserPush.complete"; id: number; status: number }
  | { action: "execute"; context: ConnectionContext; request: unknown }
  | { action: "authenticate"; context: AuthContext }
  | { action: "session.resolve"; sessionDigest: string }
  | { action: "session.create"; credentialDigest: string }
  | { action: "session.end"; context: AuthenticatedContext }
  | {
      action: "credentials.configure";
      credentials: { principalId: string; digest: string }[];
    }
  | { action: "oauth.initialize" }
  | {
      action: "oauth.family.create";
      state: OAuthRefreshFamily;
      activeCredentialReferences: string[];
    }
  | { action: "oauth.family.rotate"; rotation: OAuthRefreshRotation }
  | { action: "mcp.cursor.create"; identity: Wire.Json; offset: number }
  | { action: "mcp.catalog"; name?: string }
  | { action: "mcp.cursor.read"; identity: Wire.Json; cursor: string }
  | { action: "disconnect"; serviceNodeId: string }
  | {
      action: "asset";
      context: AuthenticatedContext;
      uiId: string;
      releaseId: string;
      path: string;
    }
  | {
      action: "ui.currentAsset";
      context: AuthenticatedContext;
      uiId: string;
      path?: string;
    }
  | { action: "ui.entry"; context: AuthenticatedContext; uiId: string }
  | { action: "ui.resolveSlug"; slug: string }
  | { action: "ui.pointers"; uiId?: string }
  | { action: "package.catalog"; after: number; includeUis: boolean }
  | { action: "package.seed"; catalog: unknown }
  | { action: "package.pruneApps" }
  | {
      action: "package.publish";
      input: PackageUploadAuthorization;
      publisherPrincipalId: string;
    }
  | {
      action: "protocol.diagnostic";
      context: ConnectionContext;
      code: string;
      message: string;
    }
  | { action: "diagnostic"; value: Wire.Diagnostic }
  | { action: "health" }
  | { action: "retention.collect" }
  | { action: "backup"; destination: string }
  | { action: "close" };
