import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import collect, {
  createAuthorizationRequest,
  exchangeAuthorizationCode,
} from "../docs/examples/data-collector/home/tesla-fleet.mjs";
import { authorize } from "../tools/operations/tesla-fleet-authorize.mjs";

const config = { region: "eu", expectedEmail: "owner@example.com" };
const secrets = {
  tesla_client_id: "fixture-client",
  tesla_refresh_token: "fixture-seed",
};
const json = (value, status = 200) =>
  new Response(JSON.stringify(value), { status });
const tokens = (number) => ({
  access_token: "fixture-access-" + number,
  refresh_token: "fixture-refresh-" + number,
  expires_in: 3600,
  token_type: "Bearer",
});
const context = (extra = {}) => ({
  config,
  secrets,
  state: null,
  signal: new AbortController().signal,
  ...extra,
});

async function directory(t) {
  const path = await mkdtemp(join(tmpdir(), "ivy-tesla-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  t.mock.method(process, "cwd", () => path);
  return path;
}

function fixture(t, options = {}) {
  const requests = [];
  let refreshed = 0;
  let access = "fixture-access-1";
  const orders = [
    {
      referenceNumber: "RN123",
      status: "pending",
      delivery: { window: "October 10 - 20" },
    },
  ];
  t.mock.method(globalThis, "fetch", async (url, request) => {
    requests.push({ url, ...request });
    assert.equal(request.redirect, "error");
    if (url === "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token") {
      assert.equal(request.body.get("client_id"), "fixture-client");
      assert.equal(request.body.get("grant_type"), "refresh_token");
      assert.equal(request.body.has("client_secret"), false);
      const number = ++refreshed;
      access = "fixture-access-" + number;
      return options.refresh?.(number, request) ?? json(tokens(number));
    }
    assert.equal(
      new URL(url).origin,
      "https://fleet-api.prd.eu.vn.cloud.tesla.com",
    );
    assert.equal(request.headers.Authorization, "Bearer " + access);
    if (url.endsWith("/users/me"))
      return (
        options.me?.(request) ??
        json({
          response: {
            email: "OWNER@example.com",
            homeAddress: "Private address",
          },
        })
      );
    assert.ok(url.endsWith("/users/orders"));
    return options.orders?.(orders, request) ?? json({ response: orders });
  });
  return {
    requests,
    orders,
    get refreshed() {
      return refreshed;
    },
  };
}

test("Tesla Fleet verifies identity, reads orders and persists rotating tokens outside results", async (t) => {
  const path = await directory(t);
  const provider = fixture(t);
  const result = await collect(context());
  assert.equal(provider.requests.length, 3);
  assert.equal(result.data.source, "tesla-fleet-api");
  assert.deepEqual(result.data.orders, provider.orders);
  assert.deepEqual(result.data.delivery, {
    status: "not_configured",
    value: null,
  });
  assert.equal(result.data.estimatedDelivery, null);
  assert.deepEqual(result.events, []);
  assert.doesNotMatch(
    JSON.stringify(result),
    /fixture-(?:access|refresh|seed)|Private address/,
  );
  const cacheFile = join(path, ".auth", "tesla-fleet.json");
  const cache = JSON.parse(await readFile(cacheFile, "utf8"));
  assert.equal(cache.refreshToken, "fixture-refresh-1");
  if (process.platform !== "win32") {
    assert.equal((await stat(cacheFile)).mode & 0o777, 0o600);
    assert.equal((await stat(join(path, ".auth"))).mode & 0o777, 0o700);
  }
  const again = await collect(context({ state: result.state }));
  assert.equal(provider.refreshed, 1);
  assert.deepEqual(again.events, []);
});

test("Tesla Fleet saves renewal before a failed data request and uses the rotated token next time", async (t) => {
  const path = await directory(t);
  const provider = fixture(t, {
    orders: () => new Response("private fixture-access-1", { status: 503 }),
  });
  await assert.rejects(
    collect(context()),
    (error) =>
      error.code === "provider_unavailable" &&
      !error.message.includes("fixture-access"),
  );
  const file = join(path, ".auth", "tesla-fleet.json");
  const cache = JSON.parse(await readFile(file, "utf8"));
  assert.equal(cache.refreshToken, "fixture-refresh-1");
  await writeFile(file, JSON.stringify({ ...cache, expiresAt: 0 }));
  await assert.rejects(collect(context()), { code: "provider_unavailable" });
  const requests = provider.requests.filter((request) =>
    request.body?.get("grant_type"),
  );
  assert.deepEqual(
    requests.map((request) => request.body.get("refresh_token")),
    ["fixture-seed", "fixture-refresh-1"],
  );
  assert.equal(
    JSON.parse(await readFile(file, "utf8")).refreshToken,
    "fixture-refresh-2",
  );
});

test("Tesla Fleet refuses another account before fetching its orders", async (t) => {
  await directory(t);
  const provider = fixture(t, {
    me: () => json({ response: { email: "other@example.com" } }),
  });
  await assert.rejects(collect(context()), { code: "configuration_invalid" });
  assert.equal(
    provider.requests.some((request) => request.url.endsWith("/users/orders")),
    false,
  );
});

test("Tesla Fleet retries a rejected access token once and retains the replacement", async (t) => {
  const path = await directory(t);
  const provider = fixture(t);
  await collect(context());
  let calls = 0;
  t.mock.method(globalThis, "fetch", async (url, request) => {
    calls++;
    if (url.includes("/oauth2/")) {
      assert.equal(request.body.get("refresh_token"), "fixture-refresh-1");
      return json(tokens(2));
    }
    return new Response("private response", { status: 401 });
  });
  await assert.rejects(collect(context()), { code: "authentication_required" });
  assert.equal(calls, 3);
  assert.equal(provider.refreshed, 1);
  assert.equal(
    JSON.parse(await readFile(join(path, ".auth", "tesla-fleet.json"), "utf8"))
      .refreshToken,
    "fixture-refresh-2",
  );
});

test("Tesla Fleet maps only configured delivery fields and ignores order/key ordering in change events", async (t) => {
  await directory(t);
  const provider = fixture(t);
  provider.orders.push({ status: "pending", referenceNumber: "RN999" });
  const mapped = context({
    config: {
      ...config,
      orderSelector: { path: "/referenceNumber", value: "RN123" },
      deliveryPointer: "/delivery/window",
    },
  });
  const first = await collect(mapped);
  assert.equal(first.data.estimatedDelivery, "October 10 - 20");
  provider.orders.reverse();
  provider.orders[1] = {
    delivery: { window: "October 10 - 20" },
    status: "pending",
    referenceNumber: "RN123",
  };
  const unchanged = await collect({ ...mapped, state: first.state });
  assert.deepEqual(unchanged.events, []);
  provider.orders[1].delivery.window = "October 20 - 30";
  const changed = await collect({ ...mapped, state: first.state });
  assert.deepEqual(
    changed.events.map((event) => event.name),
    ["tesla.orders.changed", "tesla.delivery.changed"],
  );
  assert.equal(changed.events[1].payload.previous, "October 10 - 20");
  delete provider.orders[1].delivery;
  const missing = await collect({ ...mapped, state: changed.state });
  assert.deepEqual(missing.data.delivery, {
    status: "field_missing",
    value: null,
  });
  assert.equal(
    missing.events.some((event) => event.name === "tesla.delivery.changed"),
    false,
  );
  provider.orders.length = 0;
  const empty = await collect(mapped);
  assert.deepEqual(empty.data.delivery, {
    status: "order_not_found",
    value: null,
  });
});

test("Tesla Fleet validates configuration before HTTP and rejects corrupt caches without reusing seed tokens", async (t) => {
  const path = await directory(t);
  t.mock.method(globalThis, "fetch", () =>
    assert.fail("Must not contact Tesla"),
  );
  for (const patch of [
    { region: "invalid" },
    { expectedEmail: "" },
    { deliveryPointer: "https://other.example" },
    { orderSelector: { path: "/bad~2", value: "RN123" } },
  ])
    await assert.rejects(
      collect(context({ config: { ...config, ...patch } })),
      { code: "configuration_invalid" },
    );
  await assert.rejects(collect(context({ secrets: {} })), {
    code: "authentication_required",
  });
  await mkdir(join(path, ".auth"));
  await writeFile(join(path, ".auth", "tesla-fleet.json"), "invalid cache");
  await assert.rejects(collect(context()), { code: "configuration_invalid" });
});

test("Tesla Fleet classifies expired grants, missing permissions and unsupported API bodies", async (t) => {
  await directory(t);
  fixture(t, {
    refresh: () => new Response("private fixture-seed", { status: 400 }),
  });
  await assert.rejects(collect(context()), { code: "authentication_required" });
  fixture(t, { orders: () => new Response("private", { status: 403 }) });
  await assert.rejects(collect(context()), { code: "interaction_required" });
  fixture(t, { orders: () => json({ response: { unexpected: true } }) });
  await assert.rejects(collect(context()), { code: "provider_unavailable" });
  fixture(t, { orders: () => new Response("<html>login</html>") });
  await assert.rejects(collect(context()), { code: "provider_unavailable" });
});

test("Tesla OAuth validates callback state and sends code exchange only to Fleet authentication", async (t) => {
  const request = createAuthorizationRequest({
    clientId: "fixture-client",
    redirectUri: "https://example.com/tesla/callback",
    region: "eu",
  });
  const url = new URL(request.authorizationUrl);
  assert.equal(url.origin, "https://auth.tesla.com");
  assert.ok(url.searchParams.get("scope").includes("offline_access"));
  assert.equal(url.searchParams.get("scope").includes("vehicle_cmds"), false);
  let calls = 0;
  t.mock.method(globalThis, "fetch", async (url, options) => {
    calls++;
    assert.equal(
      url,
      "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token",
    );
    assert.equal(options.redirect, "error");
    assert.equal(options.body.get("grant_type"), "authorization_code");
    assert.equal(options.body.get("client_secret"), "fixture-secret");
    assert.equal(
      options.body.get("audience"),
      "https://fleet-api.prd.eu.vn.cloud.tesla.com",
    );
    return json(tokens(1));
  });
  for (const callback of [
    "https://example.com/tesla/callback?code=private&state=wrong",
    "https://other.example/tesla/callback?code=private&state=" + request.state,
    "https://example.com/tesla/callback?code=one&code=two&state=" +
      request.state,
  ])
    await assert.rejects(
      exchangeAuthorizationCode(request, callback, "fixture-secret"),
      { code: "authentication_required" },
    );
  assert.equal(calls, 0);
  const result = await exchangeAuthorizationCode(
    request,
    "https://example.com/tesla/callback?code=private&state=" + request.state,
    "fixture-secret",
  );
  assert.equal(result.refreshToken, "fixture-refresh-1");
  assert.equal(calls, 1);
});

test("Tesla OAuth setup stores secrets in a private file and reserves it before consuming a code", async (t) => {
  const path = await directory(t);
  const client = join(path, "client.json"),
    session = join(path, "session.json");
  const callback = join(path, "callback.txt"),
    output = join(path, "secrets.json");
  await writeFile(
    client,
    JSON.stringify({
      clientId: "fixture-client",
      clientSecret: "fixture-secret",
    }),
  );
  const begin = await authorize([
    "begin",
    "--client-file",
    client,
    "--redirect-uri",
    "https://example.com/callback",
    "--session-file",
    session,
  ]);
  const request = JSON.parse(await readFile(session, "utf8"));
  assert.equal(begin.authorizationUrl, request.authorizationUrl);
  await writeFile(
    callback,
    "https://example.com/callback?code=private&state=" + request.state,
  );
  let calls = 0;
  t.mock.method(globalThis, "fetch", async () => {
    calls++;
    return json(tokens(1));
  });
  const args = [
    "finish",
    "--client-file",
    client,
    "--session-file",
    session,
    "--callback-file",
    callback,
    "--output",
    output,
  ];
  const result = await authorize(args);
  assert.doesNotMatch(JSON.stringify(result), /fixture-refresh|fixture-secret/);
  assert.deepEqual(JSON.parse(await readFile(output, "utf8")), {
    tesla_client_id: "fixture-client",
    tesla_refresh_token: "fixture-refresh-1",
  });
  await assert.rejects(authorize(args), { code: "EEXIST" });
  assert.equal(calls, 1);
});
