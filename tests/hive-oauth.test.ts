import assert from "node:assert/strict";
import { test } from "node:test";
import { createHash, randomBytes } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createServer } from "node:net";
import { once } from "node:events";
import { HiveServer } from "../services/hive/src/server.js";
import { HiveClient } from "../packages/sdk/src/client.js";
import { digest } from "../packages/contracts/src/canonical.js";
import {
  HiveOAuth,
  refreshTokenInactivityLifetimeMs,
} from "../services/hive/src/oauth.js";
import type {
  OAuthRefreshFamily,
  OAuthRefreshRotation,
} from "../services/hive/src/worker-protocol.js";
import { IvyError } from "../packages/contracts/src/errors.js";

class MemoryFamilies {
  readonly states = new Map<string, OAuthRefreshFamily>();
  async create(
    state: OAuthRefreshFamily,
    activeCredentialReferences: string[],
  ): Promise<void> {
    const active = new Set(activeCredentialReferences),
      now = Date.now();
    for (const [key, value] of this.states)
      if (
        value.revoked ||
        value.expiresAt <= now ||
        !active.has(value.credentialReference)
      )
        this.states.delete(key);
    if (this.states.size >= 1024)
      throw new IvyError(
        "limit_exceeded",
        "OAuth refresh-family capacity is full.",
      );
    if (this.states.has(state.familyDigest))
      throw new IvyError(
        "target_conflict",
        "OAuth refresh family identity already exists.",
      );
    this.states.set(state.familyDigest, structuredClone(state));
  }
  async rotate(rotation: OAuthRefreshRotation): Promise<void> {
    const state = this.states.get(rotation.familyDigest),
      now = Date.now();
    if (!state)
      throw new IvyError(
        "unauthenticated",
        "OAuth refresh token was already consumed or revoked.",
      );
    const current =
      !state.revoked &&
      state.expiresAt > now &&
      state.credentialReference === rotation.credentialReference &&
      state.clientId === rotation.clientId &&
      state.resource === rotation.resource &&
      state.scope === rotation.scope &&
      state.generation === rotation.presentedGeneration &&
      state.currentTokenDigest === rotation.presentedTokenDigest;
    if (!current) {
      state.revoked = true;
      throw new IvyError(
        "unauthenticated",
        "OAuth refresh token was already consumed or revoked.",
      );
    }
    state.generation = rotation.nextGeneration;
    state.currentTokenDigest = rotation.nextTokenDigest;
    state.expiresAt = rotation.nextExpiresAt;
  }
}

function directFixture(families = new MemoryFamilies()) {
  const credential = {
    principalId: "direct-user",
    digest: digest("direct-oauth-credential"),
  };
  const oauth = new HiveOAuth(
    new URL("https://oauth.test/ivy"),
    [credential],
    families,
  );
  const secrets = {
    signingKey: "11".repeat(32),
    referenceKey: "22".repeat(32),
  };
  oauth.initialize(secrets);
  const redirectUri = "http://127.0.0.1/callback";
  const clientId = String(
    oauth.register({
      client_name: "Direct fixture",
      redirect_uris: [redirectUri],
      token_endpoint_auth_method: "none",
    })["client_id"],
  );
  const verifier = "v".repeat(64),
    challenge = createHash("sha256").update(verifier).digest("base64url");
  const begin = () => {
    const url = new URL("https://oauth.test/ivy/oauth/authorize");
    for (const [key, value] of Object.entries({
      response_type: "code",
      client_id: clientId,
      redirect_uri: redirectUri,
      state: "s".repeat(48),
      code_challenge: challenge,
      code_challenge_method: "S256",
      resource: oauth.resource,
      scope: "hive",
    }))
      url.searchParams.set(key, value);
    return oauth.begin(url).transaction;
  };
  const code = (transaction = begin()) =>
    new URL(oauth.authorize(transaction, credential.digest)).searchParams.get(
      "code",
    )!;
  const exchange = (authorizationCode = code()) =>
    oauth.token(
      new URLSearchParams({
        grant_type: "authorization_code",
        code: authorizationCode,
        client_id: clientId,
        redirect_uri: redirectUri,
        code_verifier: verifier,
        resource: oauth.resource,
      }),
    );
  const refresh = (token: string) =>
    oauth.token(
      new URLSearchParams({
        grant_type: "refresh_token",
        refresh_token: token,
        client_id: clientId,
        resource: oauth.resource,
      }),
    );
  return {
    oauth,
    families,
    credential,
    secrets,
    redirectUri,
    clientId,
    verifier,
    begin,
    code,
    exchange,
    refresh,
  };
}

test("OAuth binds ivy and ivy_dev tokens to their own MCP resources", async () => {
  const f = directFixture();
  assert.equal(f.oauth.protectedResourceMetadata(f.oauth.developmentResource).resource, f.oauth.developmentResource);
  const authorize = new URL("https://oauth.test/ivy/oauth/authorize");
  for (const [key, value] of Object.entries({
    response_type: "code", client_id: f.clientId, redirect_uri: f.redirectUri,
    state: "s".repeat(48), code_challenge: createHash("sha256").update(f.verifier).digest("base64url"),
    code_challenge_method: "S256", resource: f.oauth.developmentResource, scope: "hive",
  })) authorize.searchParams.set(key, value);
  const transaction = f.oauth.begin(authorize).transaction;
  const code = new URL(f.oauth.authorize(transaction, f.credential.digest)).searchParams.get("code")!;
  const tokens = await f.oauth.token(new URLSearchParams({
    grant_type: "authorization_code", code, client_id: f.clientId, redirect_uri: f.redirectUri,
    code_verifier: f.verifier, resource: f.oauth.developmentResource,
  }));
  const access = String(tokens["access_token"]);
  assert.equal(f.oauth.authenticate(access, f.oauth.developmentResource)?.principalId, f.credential.principalId);
  assert.throws(() => f.oauth.authenticate(access, f.oauth.resource), { code: "unauthenticated" });
});

test("Hive OAuth discovery, DCR, authorization-code PKCE and refresh tokens authenticate the MCP principal", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-oauth-")),
    credential = "synthetic-oauth-user-credential";
  const reservation = createServer();
  reservation.listen(0, "127.0.0.1");
  await once(reservation, "listening");
  const reserved = reservation.address();
  assert.ok(reserved && typeof reserved !== "string");
  const port = reserved.port;
  await new Promise<void>((resolve, reject) =>
    reservation.close((error) => {
      if (error) reject(error);
      else resolve();
    }),
  );
  const origin = `http://127.0.0.1:${port}`,
    base = origin + "/ivy",
    resource = base + "/mcp";
  const configured = [
      { principalId: "oauth-user", digest: digest(credential) },
    ],
    options = {
      filename: join(root, "hive.sqlite"),
      publicBaseUrl: base,
      listenHost: "127.0.0.1",
      listenPort: port,
      version: "test",
      buildId: digest("oauth-test"),
      credentials: configured,
    };
  let hive = new HiveServer(options);
  await hive.start();
  t.after(async () => {
    await hive.close();
    await rm(root, { recursive: true, force: true });
  });

  const protectedMetadata = (await fetch(
    base + "/.well-known/oauth-protected-resource",
  ).then((response) => response.json())) as Record<string, unknown>;
  assert.equal(protectedMetadata.resource, resource);
  const developmentMetadata = (await fetch(
    base + "/.well-known/oauth-protected-resource/mcp-dev",
  ).then((response) => response.json())) as Record<string, unknown>;
  assert.equal(developmentMetadata.resource, base + "/mcp-dev");
  assert.deepEqual(protectedMetadata.authorization_servers, [origin]);
  const serverMetadata = (await fetch(
    origin + "/.well-known/oauth-authorization-server",
  ).then((response) => response.json())) as Record<string, unknown>;
  assert.equal(serverMetadata.issuer, origin);
  assert.equal(
    serverMetadata.authorization_response_iss_parameter_supported,
    true,
  );
  assert.deepEqual(serverMetadata.code_challenge_methods_supported, ["S256"]);

  const redirectUri = "http://127.0.0.1/callback";
  const registrationResponse = await fetch(base + "/oauth/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      client_name: "OAuth fixture",
      redirect_uris: [redirectUri],
      token_endpoint_auth_method: "none",
    }),
  });
  assert.equal(registrationResponse.status, 201);
  const registration = (await registrationResponse.json()) as {
    client_id: string;
  };
  const verifier = randomBytes(48).toString("base64url"),
    challenge = createHash("sha256").update(verifier).digest("base64url"),
    state = randomBytes(32).toString("base64url");
  const authorize = new URL(base + "/oauth/authorize");
  for (const [key, value] of Object.entries({
    response_type: "code",
    client_id: registration.client_id,
    redirect_uri: redirectUri,
    state,
    code_challenge: challenge,
    code_challenge_method: "S256",
    resource,
    scope: "hive",
  }))
    authorize.searchParams.set(key, value);
  const page = await fetch(authorize).then((response) => response.text());
  const transaction = page.match(
    /name="transaction" value="([A-Za-z0-9_-]+)"/,
  )?.[1];
  assert.ok(transaction);
  const approval = await fetch(base + "/oauth/authorize", {
    method: "POST",
    redirect: "manual",
    headers: { origin, "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ transaction, credential }),
  });
  assert.equal(approval.status, 303);
  const callback = new URL(approval.headers.get("location")!);
  assert.equal(callback.origin + callback.pathname, redirectUri);
  assert.equal(callback.searchParams.get("state"), state);
  assert.equal(callback.searchParams.get("iss"), origin);
  const code = callback.searchParams.get("code");
  assert.ok(code);
  const tokenResponse = await fetch(base + "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      code,
      client_id: registration.client_id,
      redirect_uri: redirectUri,
      code_verifier: verifier,
      resource,
    }),
  });
  assert.equal(tokenResponse.status, 200);
  const tokens = (await tokenResponse.json()) as {
    access_token: string;
    refresh_token: string;
    expires_in: number;
  };
  assert.equal(tokens.expires_in, 900);
  const encoded = Buffer.from(
    tokens.access_token.split(".")[1]!,
    "base64url",
  ).toString("utf8");
  assert.equal(encoded.includes(credential), false);
  assert.equal(encoded.includes(digest(credential)), false);
  assert.match(tokens.access_token, /^ivyoa2\./);
  const parts = tokens.access_token.split("."),
    payload = JSON.parse(encoded) as Record<string, unknown>;
  payload["expiresAt"] = Number(payload["expiresAt"]) + 86_400_000;
  const modified =
    parts[0] +
    "." +
    Buffer.from(JSON.stringify(payload)).toString("base64url") +
    "." +
    parts[2];
  for (const rejected of [
    "ivyoa1." + parts.slice(1).join("."),
    modified,
    tokens.refresh_token,
  ]) {
    await assert.rejects(
      new HiveClient(base, { credential: rejected }).request(
        "system.status",
        {},
      ),
      (error: unknown) =>
        (error as { code?: string }).code === "unauthenticated",
    );
  }
  const status = await new HiveClient(base, {
    credential: tokens.access_token,
  }).request("system.status", {});
  assert.equal(status.callerPrincipalId, "oauth-user");
  const refreshed = await fetch(base + "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: tokens.refresh_token,
      client_id: registration.client_id,
      resource,
    }),
  });
  assert.equal(refreshed.status, 200);
  const rotated = (await refreshed.json()) as {
    access_token: string;
    refresh_token: string;
  };
  await hive.close();
  hive = new HiveServer(options);
  await hive.start();
  assert.equal(
    (
      await new HiveClient(base, { credential: rotated.access_token }).request(
        "system.status",
        {},
      )
    ).callerPrincipalId,
    "oauth-user",
  );
  const afterRestartResponse = await fetch(base + "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: rotated.refresh_token,
      client_id: registration.client_id,
      resource,
    }),
  });
  assert.equal(afterRestartResponse.status, 200);
  const afterRestart = (await afterRestartResponse.json()) as {
    access_token: string;
    refresh_token: string;
  };
  const another = (await fetch(base + "/oauth/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      client_name: "Other public client",
      redirect_uris: [redirectUri],
      token_endpoint_auth_method: "none",
    }),
  }).then((response) => response.json())) as { client_id: string };
  const wrongClient = await fetch(base + "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: tokens.refresh_token,
      client_id: another.client_id,
      resource,
    }),
  });
  assert.equal(
    wrongClient.status,
    401,
    "Refresh tokens remain bound to their registered public client.",
  );
  const wrongResource = await fetch(base + "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: afterRestart.refresh_token,
      client_id: registration.client_id,
      resource: base + "/wrong",
    }),
  });
  assert.equal(wrongResource.status, 400);
  const replay = await fetch(base + "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: rotated.refresh_token,
      client_id: registration.client_id,
      resource,
    }),
  });
  assert.equal(
    replay.status,
    401,
    "A consumed predecessor is detected after restart.",
  );
  const revokedSuccessor = await fetch(base + "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: afterRestart.refresh_token,
      client_id: registration.client_id,
      resource,
    }),
  });
  assert.equal(
    revokedSuccessor.status,
    401,
    "Predecessor reuse revokes the whole refresh family.",
  );
  await hive.setCredentials([]);
  await assert.rejects(
    new HiveClient(base, { credential: afterRestart.access_token }).request(
      "system.status",
      {},
    ),
    (error: unknown) => (error as { code?: string }).code === "unauthenticated",
  );
  await hive.setCredentials(configured);
  const removedFamily = await fetch(base + "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: afterRestart.refresh_token,
      client_id: registration.client_id,
      resource,
    }),
  });
  assert.equal(
    removedFamily.status,
    401,
    "Credential revocation removes retained refresh authority.",
  );
  assert.equal(
    (await new HiveClient(base, { credential }).request("system.status", {}))
      .callerPrincipalId,
    "oauth-user",
    "Static bearer authentication remains available.",
  );
  const challengeResponse = await fetch(resource, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  });
  assert.equal(challengeResponse.status, 401);
  assert.match(
    challengeResponse.headers.get("www-authenticate") ?? "",
    /oauth-protected-resource/,
  );
});

test("OAuth pending transactions and codes retain their exact 1024 bounds across refusal, expiry and retry", async () => {
  const originalNow = Date.now;
  let now = Date.parse("2026-01-01T00:00:00.000Z");
  Date.now = () => now;
  try {
    let f = directFixture(),
      internals = f.oauth as unknown as {
        pending: Map<string, unknown>;
        codes: Map<string, unknown>;
      };
    for (let index = 0; index < 1024; index++) f.begin();
    assert.equal(internals.pending.size, 1024);
    assert.throws(
      () => f.begin(),
      (error: unknown) =>
        error instanceof IvyError && error.code === "limit_exceeded",
    );
    now += 15 * 60_000;
    const replacement = f.begin();
    assert.equal(internals.pending.size, 1);
    assert.doesNotThrow(() => f.oauth.transaction(replacement));

    now += 1;
    f = directFixture();
    internals = f.oauth as unknown as {
      pending: Map<string, unknown>;
      codes: Map<string, unknown>;
    };
    for (let index = 0; index < 1024; index++) f.code();
    assert.equal(internals.codes.size, 1024);
    now += 60_000;
    const retry = f.begin();
    for (let index = 0; index < 8; index++)
      assert.throws(
        () => f.oauth.authorize(retry, f.credential.digest),
        (error: unknown) =>
          error instanceof IvyError && error.code === "limit_exceeded",
      );
    assert.equal(
      internals.pending.size,
      1,
      "code-capacity refusal must not consume the pending authorization",
    );
    now += 14 * 60_000;
    const admitted = f.code(retry);
    assert.equal(internals.pending.size, 0);
    assert.equal(internals.codes.size, 1);
    const tokens = await f.exchange(admitted);
    assert.equal(typeof tokens["access_token"], "string");
    assert.equal(internals.codes.size, 0);
    await assert.rejects(
      f.exchange(admitted),
      (error: unknown) =>
        error instanceof IvyError && error.code === "unauthenticated",
    );

    const credentialRetry = f.begin();
    assert.throws(
      () => f.oauth.authorize(credentialRetry, digest("wrong")),
      (error: unknown) =>
        error instanceof IvyError && error.code === "unauthenticated",
    );
    assert.doesNotThrow(() => f.oauth.transaction(credentialRetry));
    assert.ok(f.code(credentialRetry));
  } finally {
    Date.now = originalNow;
  }
});

test("rolling refresh survives its original expiry, restart and full-family capacity while concurrent reuse revokes the family", async () => {
  const originalNow = Date.now;
  let now = Date.parse("2026-01-01T00:00:00.000Z");
  Date.now = () => now;
  try {
    const f = directFixture();
    const initial = (await f.exchange()) as { refresh_token: string };
    const originalExpiry = now + refreshTokenInactivityLifetimeMs;
    for (let index = 0; index < 1023; index++)
      f.families.states.set("dummy-" + index, {
        familyDigest: "dummy-" + index,
        credentialReference: [...f.families.states.values()][0]!
          .credentialReference,
        clientId: "dummy",
        resource: f.oauth.resource,
        scope: "hive",
        generation: 1,
        currentTokenDigest: digest("dummy-token-" + index),
        expiresAt: originalExpiry + 10_000,
        revoked: false,
      });
    assert.equal(f.families.states.size, 1024);
    now = originalExpiry - 1000;
    const renewed = (await f.refresh(initial.refresh_token)) as {
      refresh_token: string;
    };
    assert.equal(
      f.families.states.size,
      1024,
      "rotation at capacity updates one family instead of allocating another",
    );
    const restarted = directFixture(f.families);
    restarted.oauth.initialize(f.secrets);
    now = originalExpiry + 1000;
    const beyondOriginal = (await restarted.refresh(renewed.refresh_token)) as {
      refresh_token: string;
    };
    const live = [...f.families.states.values()].find(
      (value) => value.clientId === f.clientId,
    )!;
    assert.equal(live.generation, 3);
    assert.equal(live.expiresAt, now + refreshTokenInactivityLifetimeMs);

    const concurrent = await Promise.allSettled([
      restarted.refresh(beyondOriginal.refresh_token),
      restarted.refresh(beyondOriginal.refresh_token),
    ]);
    assert.equal(
      concurrent.filter((result) => result.status === "fulfilled").length,
      1,
    );
    assert.equal(
      concurrent.filter((result) => result.status === "rejected").length,
      1,
    );
    const successor = (
      concurrent.find(
        (result) => result.status === "fulfilled",
      ) as PromiseFulfilledResult<Record<string, unknown>>
    ).value["refresh_token"] as string;
    await assert.rejects(
      restarted.refresh(successor),
      (error: unknown) =>
        error instanceof IvyError && error.code === "unauthenticated",
    );

    for (const [key, value] of f.families.states)
      if (value.revoked) f.families.states.delete(key);
    const reference = [...f.families.states.values()][0]!.credentialReference;
    for (let index = f.families.states.size; index < 1024; index++)
      f.families.states.set("capacity-" + index, {
        familyDigest: "capacity-" + index,
        credentialReference: reference,
        clientId: "dummy",
        resource: f.oauth.resource,
        scope: "hive",
        generation: 1,
        currentTokenDigest: digest("capacity-token-" + index),
        expiresAt: now + refreshTokenInactivityLifetimeMs,
        revoked: false,
      });
    const full = directFixture(f.families);
    await assert.rejects(
      full.exchange(),
      (error: unknown) =>
        error instanceof IvyError && error.code === "limit_exceeded",
    );
    const expired = [...f.families.states.entries()].find(
      ([, value]) => value.clientId === "dummy",
    )!;
    expired[1].expiresAt = now;
    const reclaimed = await full.exchange();
    assert.equal(typeof reclaimed["refresh_token"], "string");
    assert.equal(f.families.states.size, 1024);
  } finally {
    Date.now = originalNow;
  }
});
