import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer, request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { authorize } from "../tools/operations/tiktok-authorize.mjs";

const now = Date.UTC(2026, 8, 26, 12);
const redirect = "https://example.com/auth/tiktok/callback";
const tokenResponse = {
  access_token: "fixture-access-token",
  refresh_token: "fixture-refresh-token",
  open_id: "fixture-account-id",
  token_type: "Bearer",
  expires_in: 86400,
  refresh_expires_in: 31536000,
  scope: "user.info.basic,user.info.stats,video.list",
};

async function setup(t, { platform = "web", redirectUri = redirect } = {}) {
  t.mock.timers.enable({ apis: ["Date"], now });
  const parent = resolve(process.env.IVY_TEST_TEMP ?? tmpdir());
  const directory = await mkdtemp(join(parent, "tiktok-authorization-"));
  t.after(async () => {
    assert.equal(dirname(directory), parent);
    await rm(directory, { recursive: true, force: true });
  });
  const client = join(directory, "client.json");
  const session = join(directory, "session.json");
  const callback = join(directory, "callback.txt");
  const output = join(directory, "grant.json");
  await writeFile(
    client,
    JSON.stringify({
      clientKey: "fixture-client",
      clientSecret: "fixture-secret",
    }),
  );
  const common = ["--client-file", client, "--session-file", session];
  const initial = await authorize([
    "begin",
    ...common,
    "--platform",
    platform,
    "--redirect-uri",
    redirectUri,
  ]);
  const url = new URL(initial.authorizationUrl);
  const returned = new URL(redirectUri);
  returned.search = new URLSearchParams({
    state: url.searchParams.get("state"),
    code: "fixture-code",
  }).toString();
  await writeFile(callback, returned.href);
  return {
    client,
    session,
    callback,
    output,
    initial,
    returned,
    common,
    finish: [
      "finish",
      ...common,
      "--callback-file",
      callback,
      "--output",
      output,
    ],
  };
}

test("TikTok authorization requests separate account consent and keeps the app secret out of the URL and session", async (t) => {
  const files = await setup(t);
  const url = new URL(files.initial.authorizationUrl);
  assert.equal(url.origin, "https://www.tiktok.com");
  assert.equal(url.pathname, "/v2/auth/authorize/");
  assert.equal(url.searchParams.get("disable_auto_auth"), "1");
  assert.equal(url.searchParams.get("scope"), tokenResponse.scope);
  assert.equal(url.searchParams.get("redirect_uri"), redirect);
  assert.match(url.searchParams.get("state"), /^[A-Za-z0-9_-]{43}$/);
  assert.ok(!JSON.stringify(files.initial).includes("fixture-secret"));
  assert.ok(
    !(await readFile(files.session, "utf8")).includes("fixture-secret"),
  );
  if (process.platform !== "win32")
    assert.equal((await stat(files.session)).mode & 0o777, 0o600);
});

test("Desktop authorization keeps the verifier private and uses TikTok's hexadecimal SHA-256 challenge", async (t) => {
  const redirectUri = "http://127.0.0.1:45678/callback/";
  const files = await setup(t, { platform: "desktop", redirectUri });
  const request = JSON.parse(await readFile(files.session, "utf8"));
  const url = new URL(files.initial.authorizationUrl);
  assert.match(request.codeVerifier, /^[A-Za-z0-9_-]{43}$/);
  assert.equal(url.searchParams.get("code_challenge_method"), "S256");
  assert.equal(
    url.searchParams.get("code_challenge"),
    createHash("sha256").update(request.codeVerifier).digest("hex"),
  );
  assert.ok(!JSON.stringify(files.initial).includes(request.codeVerifier));
  t.mock.method(globalThis, "fetch", async (_url, options) => {
    assert.equal(options.body.get("code_verifier"), request.codeVerifier);
    assert.equal(options.body.get("redirect_uri"), redirectUri);
    return new Response(JSON.stringify(tokenResponse));
  });
  assert.equal((await authorize(files.finish)).complete, true);
});

test("Desktop grants reject non-loopback, missing-port and non-static redirects before saving a session", async (t) => {
  const files = await setup(t);
  for (const redirectUri of [
    "https://example.com:45678/callback/",
    "http://127.0.0.1/callback/",
    "http://127.0.0.1:0/callback/",
    "http://localhost:45678/callback/?query=1",
    "http://localhost:45678/callback/#fragment",
    "http://user@localhost:45678/callback/",
  ])
    await assert.rejects(
      authorize([
        "begin",
        "--client-file",
        files.client,
        "--session-file",
        files.session + ".invalid",
        "--platform",
        "desktop",
        "--redirect-uri",
        redirectUri,
      ]),
      /registered static loopback/,
    );
  await assert.rejects(readFile(files.session + ".invalid"), {
    code: "ENOENT",
  });
  await assert.rejects(
    authorize([
      "begin",
      "--client-file",
      files.client,
      "--session-file",
      files.session + ".web",
      "--redirect-uri",
      "http://127.0.0.1:45678/callback/",
    ]),
    /static HTTPS/,
  );
});

async function availablePort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

function callbackRequest(url, { host, method = "GET" } = {}) {
  return new Promise((resolve, reject) => {
    const request = httpRequest(
      url,
      {
        method,
        headers: host ? { Host: host } : {},
      },
      (response) => {
        let body = "";
        response.setEncoding("utf8");
        response.on("data", (data) => {
          body += data;
        });
        response.on("end", () =>
          resolve({
            status: response.statusCode,
            body,
            headers: response.headers,
          }),
        );
      },
    );
    request.on("error", reject);
    request.end();
  });
}

async function desktopListenerSetup(t) {
  const port = await availablePort();
  const files = await setup(t, {
    platform: "desktop",
    redirectUri: `http://127.0.0.1:${port}/callback/`,
  });
  files.capture = join(dirname(files.session), "captured.txt");
  files.listen = [
    "listen",
    "--session-file",
    files.session,
    "--callback-file",
    files.capture,
  ];
  files.abort = new AbortController();
  t.after(() => files.abort.abort());
  return files;
}

test("Loopback capture rejects foreign hosts, methods, paths and state before privately saving one valid callback", async (t) => {
  const files = await desktopListenerSetup(t);
  const ready = Promise.withResolvers();
  const capture = authorize(files.listen, {
    onListening: ready.resolve,
    signal: files.abort.signal,
  });
  void capture.catch(ready.reject);
  assert.deepEqual(await ready.promise, {
    listening: true,
    redirectUri: files.returned.origin + files.returned.pathname,
  });
  const wrongState = new URL(files.returned);
  wrongState.searchParams.set("state", "wrong-state");
  const wrongPath = new URL(files.returned);
  wrongPath.pathname = "/other";
  for (const [url, options] of [
    [files.returned, { host: "unrelated.example" }],
    [files.returned, { method: "POST" }],
    [wrongState, {}],
    [wrongPath, {}],
  ]) {
    const response = await callbackRequest(url, options);
    assert.equal(response.status, 400);
    assert.ok(!response.body.includes("fixture-code"));
    assert.equal(await readFile(files.capture, "utf8"), "");
  }
  const response = await callbackRequest(files.returned);
  assert.equal(response.status, 200);
  assert.equal(response.headers["cache-control"], "no-store");
  assert.ok(!response.body.includes("fixture-code"));
  assert.deepEqual(await capture, {
    callbackFile: files.capture,
    captured: true,
  });
  assert.equal(
    (await readFile(files.capture, "utf8")).trim(),
    files.returned.href,
  );
  await assert.rejects(callbackRequest(files.returned));
  await assert.rejects(authorize(files.listen), { code: "EEXIST" });
});

test("A rejected owner consent closes the loopback listener without saving a code or provider error text", async (t) => {
  const files = await desktopListenerSetup(t);
  const ready = Promise.withResolvers();
  const capture = authorize(files.listen, {
    onListening: ready.resolve,
    signal: files.abort.signal,
  });
  void capture.catch(ready.reject);
  const rejected = assert.rejects(capture, /did not grant access/);
  await ready.promise;
  const denied = new URL(files.returned);
  denied.searchParams.delete("code");
  denied.searchParams.set("error", "access_denied");
  denied.searchParams.set("error_description", "private-provider-text");
  const response = await callbackRequest(denied);
  assert.equal(response.status, 200);
  assert.ok(!response.body.includes("private-provider-text"));
  await rejected;
  assert.equal(await readFile(files.capture, "utf8"), "");
});

test("The loopback capture stops at the authorization deadline and never overwrites an existing callback", async (t) => {
  const files = await desktopListenerSetup(t);
  const request = JSON.parse(await readFile(files.session, "utf8"));
  request.createdAt = now - 600000 + 25;
  await writeFile(files.session, JSON.stringify(request));
  await assert.rejects(authorize(files.listen), /session expired/);
  assert.equal(await readFile(files.capture, "utf8"), "");
  await assert.rejects(callbackRequest(files.returned));
  request.createdAt = now;
  await writeFile(files.session, JSON.stringify(request));
  await writeFile(files.capture, "preserve existing callback");
  await assert.rejects(authorize(files.listen), { code: "EEXIST" });
  assert.equal(
    await readFile(files.capture, "utf8"),
    "preserve existing callback",
  );
});

test("TikTok callback state, destination and repeated parameters fail before any token request", async (t) => {
  const files = await setup(t);
  let calls = 0;
  t.mock.method(globalThis, "fetch", async () => {
    calls++;
    throw new Error("Unexpected request");
  });
  const callbacks = [
    new URL(files.returned),
    new URL(files.returned),
    new URL(files.returned),
    new URL(files.returned),
  ];
  callbacks[0].searchParams.set("state", "different-state");
  callbacks[1].host = "other.example.com";
  callbacks[2].pathname = "/different/callback";
  callbacks[3].searchParams.append("code", "another-code");
  for (const callback of callbacks) {
    await writeFile(files.callback, callback.href);
    await assert.rejects(authorize(files.finish), /does not match/);
  }
  assert.equal(calls, 0);
  await assert.rejects(readFile(files.output), { code: "ENOENT" });
});

test("TikTok expired sessions and another app cannot consume an authorization code", async (t) => {
  const files = await setup(t);
  let calls = 0;
  t.mock.method(globalThis, "fetch", async () => {
    calls++;
    throw new Error("Unexpected request");
  });
  const request = JSON.parse(await readFile(files.session, "utf8"));
  for (const invalid of [
    { ...request, clientKey: "different-client" },
    { ...request, createdAt: now - 600000 },
    { ...request, createdAt: now + 1 },
  ]) {
    await writeFile(files.session, JSON.stringify(invalid));
    await assert.rejects(authorize(files.finish), /invalid or expired/);
  }
  assert.equal(calls, 0);
});

test("TikTok token exchange persists account-specific grants without returning credentials", async (t) => {
  const files = await setup(t);
  let calls = 0;
  t.mock.method(globalThis, "fetch", async (url, options) => {
    calls++;
    assert.equal(url, "https://open.tiktokapis.com/v2/oauth/token/");
    assert.equal(options.method, "POST");
    assert.equal(options.redirect, "error");
    assert.deepEqual(Object.fromEntries(options.body), {
      client_key: "fixture-client",
      client_secret: "fixture-secret",
      grant_type: "authorization_code",
      code: "fixture-code",
      redirect_uri: redirect,
    });
    assert.ok(options.signal instanceof AbortSignal);
    assert.deepEqual(JSON.parse(await readFile(files.output, "utf8")), {});
    assert.equal(
      JSON.parse(await readFile(files.session + ".exchange", "utf8")).output,
      files.output,
    );
    return new Response(JSON.stringify(tokenResponse));
  });
  const result = await authorize(files.finish);
  assert.equal(result.complete, true);
  const grant = JSON.parse(await readFile(files.output, "utf8"));
  assert.equal(grant.openId, tokenResponse.open_id);
  assert.equal(grant.refreshToken, tokenResponse.refresh_token);
  assert.equal(grant.accessToken, tokenResponse.access_token);
  assert.equal(Date.parse(grant.expiresAt) - now, 86400000);
  for (const secret of [
    "fixture-secret",
    "fixture-code",
    tokenResponse.access_token,
    tokenResponse.refresh_token,
  ])
    assert.ok(!JSON.stringify(result).includes(secret));
  if (process.platform !== "win32")
    assert.equal((await stat(files.output)).mode & 0o777, 0o600);
  // A second output path does not permit a replay of the same one-time code.
  const replay = [
    ...files.finish.slice(0, -1),
    join(dirname(files.output), "replay.json"),
  ];
  await assert.rejects(authorize(replay), /already started a token exchange/);
  assert.equal(calls, 1);
});

test("TikTok will not consume a code when the private grant destination already exists", async (t) => {
  const files = await setup(t);
  await writeFile(files.output, "preserve existing grant");
  t.mock.method(globalThis, "fetch", async () => {
    assert.fail("No token request expected");
  });
  await assert.rejects(authorize(files.finish), { code: "EEXIST" });
  assert.equal(await readFile(files.output, "utf8"), "preserve existing grant");
  await assert.rejects(readFile(files.session + ".exchange"), {
    code: "ENOENT",
  });
});

test("TikTok refused, malformed and ambiguous exchanges redact provider text and cannot be retried", async (t) => {
  for (const kind of ["refused", "malformed", "ambiguous"]) {
    await t.test(kind, async (t) => {
      const files = await setup(t);
      let calls = 0;
      t.mock.method(globalThis, "fetch", async () => {
        calls++;
        if (kind === "ambiguous")
          throw new Error("fixture-secret in network error");
        return new Response(
          JSON.stringify(
            kind === "refused"
              ? {
                  error: "invalid_grant",
                  error_description: "fixture-secret in provider error",
                }
              : { ...tokenResponse, expires_in: -1 },
          ),
          { status: kind === "refused" ? 400 : 200 },
        );
      });
      await assert.rejects(authorize(files.finish), (error) => {
        assert.ok(error.safe);
        assert.ok(!error.message.includes("fixture-secret"));
        return true;
      });
      await assert.rejects(
        authorize([
          ...files.finish.slice(0, -1),
          join(dirname(files.output), "retry.json"),
        ]),
        /already started/,
      );
      assert.equal(calls, 1);
      assert.deepEqual(JSON.parse(await readFile(files.output, "utf8")), {});
    });
  }
});

test("TikTok preserves a partial grant but does not label it ready for the collector", async (t) => {
  const files = await setup(t);
  t.mock.method(
    globalThis,
    "fetch",
    async () =>
      new Response(
        JSON.stringify({ ...tokenResponse, scope: "user.info.basic" }),
      ),
  );
  const result = await authorize(files.finish);
  assert.equal(result.complete, false);
  assert.match(result.message, /required scopes are missing/);
  const grant = JSON.parse(await readFile(files.output, "utf8"));
  assert.deepEqual(grant.scopes, ["user.info.basic"]);
  assert.equal(grant.refreshToken, tokenResponse.refresh_token);
});
