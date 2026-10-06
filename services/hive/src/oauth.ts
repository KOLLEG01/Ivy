import {
  createHash,
  createHmac,
  randomBytes,
  timingSafeEqual,
} from "node:crypto";
import {
  canonical,
  digest,
} from "../../../packages/contracts/src/canonical.js";
import {
  IvyError,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import type {
  OAuthRefreshFamily,
  OAuthRefreshRotation,
} from "./worker-protocol.js";

interface Credential {
  principalId: string;
  digest: string;
}
interface Client {
  version: 1;
  redirectUris: string[];
  clientName: string;
}
interface PendingAuthorization extends Client {
  clientId: string;
  redirectUri: string;
  state: string;
  challenge: string;
  scope: string;
  resource: string;
  expiresAt: number;
}
interface Code extends PendingAuthorization {
  principalId: string;
  credentialDigest: string;
}
interface TokenBase {
  version: 2;
  type: "access" | "refresh";
  credentialReference: string;
  clientId: string;
  resource: string;
  scope: string;
  issuedAt: number;
  expiresAt: number;
  nonce: string;
}
interface AccessTokenPayload extends TokenBase {
  type: "access";
}
interface RefreshTokenPayload extends TokenBase {
  type: "refresh";
  familyId: string;
  generation: number;
}
type TokenPayload = AccessTokenPayload | RefreshTokenPayload;

export interface OAuthSecrets {
  signingKey: string;
  referenceKey: string;
}
export interface OAuthFamilyStore {
  create(
    state: OAuthRefreshFamily,
    activeCredentialReferences: string[],
  ): Promise<void>;
  rotate(rotation: OAuthRefreshRotation): Promise<void>;
}

// Hive's current MCP surface is an agent capability, not a user-facing CRUD API.
// Advertise one honest full-access scope until per-tool authorization exists.
const supportedScopes = ["hive"];
const authorizationTransactionLifetimeMs = 15 * 60_000;
const accessTokenLifetimeMs = 15 * 60_000;
// Intended clients persist refresh credentials; one year of inactivity is the practical
// local default. Every successful rotation renews it and there is no family-age cap.
export const refreshTokenInactivityLifetimeMs = 365 * 24 * 60 * 60_000;
const base64 = (bytes: Buffer | string) =>
  Buffer.from(bytes).toString("base64url");
function parseObject(value: string): Record<string, unknown> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(Buffer.from(value, "base64url").toString("utf8"));
  } catch {
    throw new IvyError("unauthenticated", "Invalid OAuth credential.");
  }
  requireThat(
    parsed !== null && typeof parsed === "object" && !Array.isArray(parsed),
    "unauthenticated",
    "Invalid OAuth credential.",
  );
  return parsed as Record<string, unknown>;
}
function safeRedirect(value: unknown): string {
  requireThat(
    typeof value === "string" &&
      value.length > 0 &&
      value.length <= 2048 &&
      !value.includes("\\"),
    "invalid_arguments",
    "OAuth redirect URI is invalid.",
  );
  const url = new URL(value),
    loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  requireThat(
    (url.protocol === "https:" || (url.protocol === "http:" && loopback)) &&
      !url.username &&
      !url.password &&
      !url.hash,
    "invalid_arguments",
    "OAuth redirects require HTTPS or an exact loopback HTTP host.",
  );
  return url.href;
}
function scope(value: string | null): string {
  const requested = (value || supportedScopes.join(" "))
    .split(/\s+/)
    .filter(Boolean);
  requireThat(
    requested.length > 0 &&
      new Set(requested).size === requested.length &&
      requested.every((item) => supportedScopes.includes(item)),
    "invalid_arguments",
    "OAuth scope is unsupported.",
  );
  return requested.join(" ");
}
function escapeHtml(value: string): string {
  return value.replace(
    /[&<>"']/g,
    (character) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        character
      ]!,
  );
}

export function oauthPage(
  basePath: string,
  transaction: string,
  clientName: string,
  requestedScope: string,
  rejected = false,
): string {
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" type="image/svg+xml" href="${escapeHtml(basePath + '/mcp-icon.svg')}"><title>Authorize Ivy</title><style>body{font-family:system-ui,sans-serif;max-width:34rem;margin:8vh auto;padding:1.5rem;color:#171717}main{border:1px solid #ddd;border-radius:14px;padding:1.5rem}label,input,button{display:block;width:100%;box-sizing:border-box}input{padding:.75rem;margin:.5rem 0 1rem}button{padding:.8rem;font-weight:650}.error{color:#a00}</style></head><body><main><h1>Authorize Ivy</h1><p><strong>${escapeHtml(clientName)}</strong> requests full access to this Hive.</p><p>Permissions: ${escapeHtml(requestedScope)}</p>${rejected ? '<p class="error" role="alert">That Hive credential was not accepted.</p>' : ""}<form method="post" action="${escapeHtml(basePath)}/oauth/authorize"><input type="hidden" name="transaction" value="${escapeHtml(transaction)}"><label for="credential">Hive credential</label><input id="credential" name="credential" type="password" required maxlength="4096" autocomplete="current-password"><button type="submit">Authorize</button></form></main></body></html>`;
}

export class HiveOAuth {
  readonly issuer: string;
  readonly resource: string;
  readonly developmentResource: string;
  private credentials: Credential[] = [];
  private byDigest = new Map<string, string>();
  private byReference = new Map<string, Credential>();
  private pending = new Map<string, PendingAuthorization>();
  private codes = new Map<string, Code>();
  private signingKey: Buffer | null = null;
  private referenceKey: Buffer | null = null;
  constructor(
    readonly base: URL,
    credentials: Credential[],
    private readonly families: OAuthFamilyStore,
  ) {
    this.issuer = base.origin;
    this.resource = new URL(
      base.pathname.replace(/\/$/, "") + "/mcp",
      base.origin,
    ).href;
    this.developmentResource = new URL(
      base.pathname.replace(/\/$/, "") + "/mcp-dev",
      base.origin,
    ).href;
    this.setCredentials(credentials);
  }
  initialize(secrets: OAuthSecrets): void {
    requireThat(
      /^[a-f0-9]{64}$/.test(secrets.signingKey) &&
        /^[a-f0-9]{64}$/.test(secrets.referenceKey) &&
        secrets.signingKey !== secrets.referenceKey,
      "storage_invalid",
      "OAuth key initialization is invalid.",
    );
    this.signingKey = Buffer.from(secrets.signingKey, "hex");
    this.referenceKey = Buffer.from(secrets.referenceKey, "hex");
    this.indexCredentials();
  }
  setCredentials(credentials: Credential[]): void {
    this.credentials = credentials.map((value) => ({ ...value }));
    this.byDigest = new Map(
      credentials.map((value) => [value.digest, value.principalId]),
    );
    this.indexCredentials();
  }
  private indexCredentials(): void {
    this.byReference.clear();
    if (!this.referenceKey) return;
    for (const credential of this.credentials)
      this.byReference.set(this.reference(credential.digest), credential);
  }
  private reference(credentialDigest: string): string {
    requireThat(
      this.referenceKey,
      "service_unavailable",
      "OAuth is not initialized.",
    );
    return (
      "ivyr2." +
      createHmac("sha256", this.referenceKey)
        .update("ivy/oauth/credential-reference/v2\0" + credentialDigest)
        .digest("base64url")
    );
  }
  private activeReferences(): string[] {
    return [...this.byReference.keys()];
  }
  protectedResourceMetadata(resource = this.resource) {
    requireThat(this.isResource(resource), "invalid_arguments", "Unknown Hive MCP resource.");
    return {
      resource,
      authorization_servers: [this.issuer],
      scopes_supported: supportedScopes,
    };
  }
  private isResource(resource: string): boolean {
    return resource === this.resource || resource === this.developmentResource;
  }
  authorizationServerMetadata(basePath: string) {
    const endpoint = (path: string) =>
      new URL(basePath + path, this.base.origin).href;
    return {
      issuer: this.issuer,
      authorization_response_iss_parameter_supported: true,
      authorization_endpoint: endpoint("/oauth/authorize"),
      token_endpoint: endpoint("/oauth/token"),
      registration_endpoint: endpoint("/oauth/register"),
      response_types_supported: ["code"],
      grant_types_supported: ["authorization_code", "refresh_token"],
      token_endpoint_auth_methods_supported: ["none"],
      code_challenge_methods_supported: ["S256"],
      scopes_supported: supportedScopes,
    };
  }
  register(value: unknown): Record<string, unknown> {
    requireThat(
      value !== null && typeof value === "object" && !Array.isArray(value),
      "invalid_arguments",
      "OAuth client registration requires an object.",
    );
    const body = value as Record<string, unknown>,
      redirects = body["redirect_uris"];
    requireThat(
      Array.isArray(redirects) &&
        redirects.length > 0 &&
        redirects.length <= 16 &&
        redirects.every((item) => typeof item === "string"),
      "invalid_arguments",
      "OAuth client registration requires redirect_uris.",
    );
    requireThat(
      body["token_endpoint_auth_method"] === undefined ||
        body["token_endpoint_auth_method"] === "none",
      "invalid_arguments",
      "Hive supports public PKCE clients only.",
    );
    const client: Client = {
      version: 1,
      redirectUris: redirects.map(safeRedirect),
      clientName:
        typeof body["client_name"] === "string" &&
        body["client_name"].length <= 256
          ? body["client_name"]
          : "OpenAI MCP client",
    };
    const clientId = "ivyc1." + base64(canonical(client));
    requireThat(
      clientId.length <= 4096,
      "limit_exceeded",
      "OAuth client registration is too large.",
    );
    return {
      client_id: clientId,
      client_id_issued_at: Math.floor(Date.now() / 1000),
      redirect_uris: client.redirectUris,
      client_name: client.clientName,
      token_endpoint_auth_method: "none",
    };
  }
  private client(clientId: string): Client {
    requireThat(
      clientId.startsWith("ivyc1.") && clientId.length <= 4096,
      "invalid_arguments",
      "Unknown OAuth client.",
    );
    const value = parseObject(clientId.slice(6));
    requireThat(
      value["version"] === 1 &&
        Array.isArray(value["redirectUris"]) &&
        value["redirectUris"].length > 0 &&
        value["redirectUris"].length <= 16 &&
        value["redirectUris"].every(
          (item) => typeof item === "string" && safeRedirect(item) === item,
        ) &&
        typeof value["clientName"] === "string" &&
        value["clientName"].length <= 256,
      "invalid_arguments",
      "Unknown OAuth client.",
    );
    return value as unknown as Client;
  }
  begin(url: URL): { transaction: string; clientName: string; scope: string } {
    requireThat(
      url.searchParams.get("response_type") === "code",
      "invalid_arguments",
      "Hive OAuth requires the authorization-code flow.",
    );
    const clientId = url.searchParams.get("client_id") ?? "",
      client = this.client(clientId),
      redirectUri = safeRedirect(url.searchParams.get("redirect_uri"));
    requireThat(
      client.redirectUris.includes(redirectUri),
      "invalid_arguments",
      "OAuth redirect URI was not registered.",
    );
    const state = url.searchParams.get("state") ?? "",
      challenge = url.searchParams.get("code_challenge") ?? "",
      resource = url.searchParams.get("resource") ?? "";
    requireThat(
      state.length > 0 &&
        state.length <= 4096 &&
        /^[A-Za-z0-9._~-]{43,128}$/.test(challenge) &&
        url.searchParams.get("code_challenge_method") === "S256",
      "invalid_arguments",
      "OAuth authorization requires state and S256 PKCE.",
    );
    requireThat(
      this.isResource(resource),
      "invalid_arguments",
      "OAuth resource does not identify this Hive MCP server.",
    );
    const requestedScope = scope(url.searchParams.get("scope")),
      transaction = randomBytes(32).toString("base64url");
    this.collect();
    requireThat(
      this.pending.size < 1024,
      "limit_exceeded",
      "OAuth authorization capacity is full.",
    );
    this.pending.set(transaction, {
      ...client,
      clientId,
      redirectUri,
      state,
      challenge,
      scope: requestedScope,
      resource,
      expiresAt: Date.now() + authorizationTransactionLifetimeMs,
    });
    return {
      transaction,
      clientName: client.clientName,
      scope: requestedScope,
    };
  }
  transaction(id: string): PendingAuthorization {
    this.collect();
    const value = this.pending.get(id);
    requireThat(
      value && value.expiresAt > Date.now(),
      "invalid_arguments",
      "OAuth authorization transaction expired.",
    );
    return value;
  }
  authorize(id: string, credentialDigest: string): string {
    const pending = this.transaction(id),
      principalId = this.byDigest.get(credentialDigest);
    requireThat(
      principalId,
      "unauthenticated",
      "A current Hive credential is required.",
    );
    this.collect();
    requireThat(
      this.codes.size < 1024,
      "limit_exceeded",
      "OAuth authorization-code capacity is full.",
    );
    this.pending.delete(id);
    const code = randomBytes(32).toString("base64url");
    this.codes.set(code, { ...pending, principalId, credentialDigest });
    const redirect = new URL(pending.redirectUri);
    redirect.searchParams.set("code", code);
    redirect.searchParams.set("state", pending.state);
    redirect.searchParams.set("iss", this.issuer);
    return redirect.href;
  }
  async token(form: URLSearchParams): Promise<Record<string, unknown>> {
    const grant = form.get("grant_type"),
      clientId = form.get("client_id") ?? "",
      resource = form.get("resource") ?? "";
    this.client(clientId);
    requireThat(
      this.isResource(resource),
      "invalid_arguments",
      "OAuth token resource does not identify this Hive MCP server.",
    );
    let credentialReference: string,
      requestedScope: string,
      refreshToken: string;
    if (grant === "authorization_code") {
      const codeValue = form.get("code") ?? "",
        code = this.codes.get(codeValue);
      this.codes.delete(codeValue);
      requireThat(
        code &&
          code.resource === resource &&
          code.expiresAt > Date.now() &&
          code.clientId === clientId &&
          code.redirectUri === safeRedirect(form.get("redirect_uri")),
        "unauthenticated",
        "OAuth authorization code is invalid or expired.",
      );
      const verifier = form.get("code_verifier") ?? "";
      requireThat(
        /^[A-Za-z0-9._~-]{43,128}$/.test(verifier) &&
          createHash("sha256").update(verifier).digest("base64url") ===
            code.challenge,
        "unauthenticated",
        "OAuth PKCE verification failed.",
      );
      requireThat(
        this.byDigest.get(code.credentialDigest) === code.principalId,
        "unauthenticated",
        "Hive credential is no longer configured.",
      );
      credentialReference = this.reference(code.credentialDigest);
      requestedScope = code.scope;
      const familyId = randomBytes(32).toString("base64url"),
        expiresAt = Date.now() + refreshTokenInactivityLifetimeMs;
      refreshToken = this.issue({
        version: 2,
        type: "refresh",
        credentialReference,
        clientId,
        resource,
        scope: requestedScope,
        issuedAt: Date.now(),
        expiresAt,
        nonce: randomBytes(16).toString("base64url"),
        familyId,
        generation: 1,
      });
      await this.families.create(
        {
          familyDigest: digest(familyId),
          credentialReference,
          clientId,
          resource,
          scope: requestedScope,
          generation: 1,
          currentTokenDigest: digest(refreshToken),
          expiresAt,
          revoked: false,
        },
        this.activeReferences(),
      );
    } else if (grant === "refresh_token") {
      const presented = form.get("refresh_token") ?? "",
        payload = this.verify(presented, "refresh");
      requireThat(
        payload.clientId === clientId,
        "unauthenticated",
        "OAuth refresh token belongs to another client.",
      );
      requireThat(payload.resource === resource, "unauthenticated", "OAuth refresh token belongs to another MCP resource.");
      credentialReference = payload.credentialReference;
      requestedScope = form.get("scope")
        ? scope(form.get("scope"))
        : payload.scope;
      requireThat(
        requestedScope
          .split(" ")
          .every((item) => payload.scope.split(" ").includes(item)),
        "invalid_arguments",
        "Refresh cannot expand OAuth scope.",
      );
      const expiresAt = Date.now() + refreshTokenInactivityLifetimeMs,
        generation = payload.generation + 1;
      refreshToken = this.issue({
        ...payload,
        issuedAt: Date.now(),
        expiresAt,
        nonce: randomBytes(16).toString("base64url"),
        scope: requestedScope,
        generation,
      });
      await this.families.rotate({
        familyDigest: digest(payload.familyId),
        credentialReference,
        clientId,
        resource,
        scope: payload.scope,
        presentedGeneration: payload.generation,
        presentedTokenDigest: digest(presented),
        nextGeneration: generation,
        nextTokenDigest: digest(refreshToken),
        nextExpiresAt: expiresAt,
      });
    } else
      throw new IvyError("invalid_arguments", "Unsupported OAuth grant type.");
    const accessToken = this.issue({
      version: 2,
      type: "access",
      credentialReference,
      clientId,
      resource,
      scope: requestedScope,
      issuedAt: Date.now(),
      expiresAt: Date.now() + accessTokenLifetimeMs,
      nonce: randomBytes(16).toString("base64url"),
    });
    return {
      access_token: accessToken,
      token_type: "Bearer",
      expires_in: 900,
      refresh_token: refreshToken,
      scope: requestedScope,
    };
  }
  authenticate(token: string, resource?: string): {
    principalId: string;
    credentialDigest: string;
    expiresAt: number;
  } | null {
    if (!token.startsWith("ivyoa2.")) return null;
    const payload = this.verify(token, "access"),
      credential = this.byReference.get(payload.credentialReference)!;
    requireThat(!resource || payload.resource === resource, "unauthenticated", "OAuth token belongs to another MCP resource.");
    return {
      principalId: credential.principalId,
      credentialDigest: credential.digest,
      expiresAt: payload.expiresAt,
    };
  }
  private issue(payload: TokenPayload): string {
    requireThat(
      this.signingKey && this.byReference.has(payload.credentialReference),
      "unauthenticated",
      "Hive credential is no longer configured.",
    );
    const encoded = base64(canonical(payload)),
      signature = createHmac("sha256", this.signingKey)
        .update(encoded)
        .digest();
    return "ivyoa2." + encoded + "." + base64(signature);
  }
  private verify<T extends TokenPayload["type"]>(
    token: string,
    type: T,
  ): Extract<TokenPayload, { type: T }> {
    const parts = token.split(".");
    requireThat(
      parts.length === 3 &&
        parts[0] === "ivyoa2" &&
        token.length <= 8192 &&
        this.signingKey,
      "unauthenticated",
      "Invalid OAuth credential.",
    );
    const expected = createHmac("sha256", this.signingKey)
        .update(parts[1]!)
        .digest(),
      actual = Buffer.from(parts[2]!, "base64url");
    requireThat(
      actual.length === expected.length && timingSafeEqual(actual, expected),
      "unauthenticated",
      "Invalid OAuth credential.",
    );
    const value = parseObject(parts[1]!);
    requireThat(
      value["version"] === 2 &&
        value["type"] === type &&
        typeof value["resource"] === "string" && this.isResource(value["resource"]) &&
        typeof value["credentialReference"] === "string" &&
        this.byReference.has(value["credentialReference"]) &&
        typeof value["clientId"] === "string" &&
        value["clientId"].length > 0 &&
        value["clientId"].length <= 4096 &&
        value["scope"] === "hive" &&
        Number.isSafeInteger(value["issuedAt"]) &&
        Number.isSafeInteger(value["expiresAt"]) &&
        Number(value["issuedAt"]) <= Date.now() + 60_000 &&
        Number(value["expiresAt"]) > Date.now() &&
        typeof value["nonce"] === "string",
      "unauthenticated",
      "OAuth credential is expired or invalid.",
    );
    if (type === "refresh")
      requireThat(
        typeof value["familyId"] === "string" &&
          /^[A-Za-z0-9_-]{43}$/.test(value["familyId"]) &&
          Number.isSafeInteger(value["generation"]) &&
          Number(value["generation"]) >= 1,
        "unauthenticated",
        "OAuth refresh credential is invalid.",
      );
    return value as unknown as Extract<TokenPayload, { type: T }>;
  }
  private collect(): void {
    const now = Date.now();
    for (const [id, value] of this.pending)
      if (value.expiresAt <= now) this.pending.delete(id);
    for (const [id, value] of this.codes)
      if (value.expiresAt <= now) this.codes.delete(id);
  }
}
