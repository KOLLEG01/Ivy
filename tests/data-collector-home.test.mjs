import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFile } from "node:fs/promises";
import heating from "../docs/examples/data-collector/home/froeling.mjs";
import laundry from "../docs/examples/data-collector/home/laundry.mjs";
import tesla from "../docs/examples/data-collector/home/tesla-delivery.mjs";

const home = new URL("../docs/examples/data-collector/home/", import.meta.url);
const signal = () => new AbortController().signal;
const credentials = {
  froeling_username: "fixture-user",
  froeling_password: "fixture-password",
};

async function heatingServer(t) {
  const requests = [];
  const values = { boilerTemp: "55,5 °C", bufferTempTop: "75.0" };
  const failures = {};
  let auth = "Bearer fixture-session";
  const server = createServer(async (request, response) => {
    let body = "";
    for await (const chunk of request) body += chunk;
    requests.push({
      path: request.url,
      method: request.method,
      headers: request.headers,
      body,
    });
    if (request.url === "/connect/v1.0/resources/login") {
      if (auth) response.setHeader("Authorization", auth);
      response.end("{}");
      return;
    }
    const component = request.url.split("/").at(-1);
    if (failures[component]) {
      response.writeHead(503);
      response.end("secret fixture response");
      return;
    }
    response.setHeader("Content-Type", "application/json");
    response.end(
      JSON.stringify(
        component === "1_100"
          ? {
              topView: {
                pictureParams: {
                  state: { name: "state", value: "Heating" },
                  bufferPumpControl: { name: "bufferPumpControl", value: "0" },
                },
              },
              stateView: [{ name: "boilerTemp", value: values.boilerTemp }],
            }
          : {
              stateView: [
                {
                  name: component === "400_4100" ? "bufferTempTop" : "flowTemp",
                  value: component === "400_4100" ? values.bufferTempTop : "45",
                },
              ],
            },
      ),
    );
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(
    () =>
      new Promise((resolve, reject) => {
        server.close((error) => (error ? reject(error) : resolve()));
        server.closeAllConnections();
      }),
  );
  return {
    requests,
    values,
    failures,
    removeAuthorization() {
      auth = null;
    },
    context: {
      config: {
        baseUrl: `http://127.0.0.1:${server.address().port}`,
        userId: "user",
        facilityId: "facility",
        thresholds: [{ component: "boiler", metric: "boilerTemp", below: 60 }],
      },
      secrets: credentials,
      state: null,
      signal: signal(),
    },
  };
}

test("Fröling logs in, reads three components and preserves localized metrics without persisting authorization", async (t) => {
  const fixture = await heatingServer(t);
  const result = await heating(fixture.context);
  assert.equal(result.data.components.boiler.values.boilerTemp, 55.5);
  assert.equal(result.data.components.boiler.values.state, "Heating");
  assert.equal(result.data.components.boiler.values.bufferPumpControl, 0);
  assert.equal(result.data.components.buffer.values.bufferTempTop, 75);
  assert.equal(result.events[0].name, "heating.threshold.crossed");
  assert.deepEqual(JSON.parse(fixture.requests[0].body), {
    osType: "web",
    username: credentials.froeling_username,
    password: credentials.froeling_password,
  });
  assert.equal(fixture.requests[0].method, "POST");
  assert.ok(
    fixture.requests
      .slice(1)
      .every(
        (r) =>
          r.method === "GET" &&
          r.headers.authorization === "Bearer fixture-session",
      ),
  );
  assert.equal(fixture.requests.length, 4);
  assert.doesNotMatch(
    JSON.stringify(result),
    /fixture-user|fixture-password|fixture-session/,
  );
});

test("Fröling threshold state prevents repeats and rearms at the exact threshold", async (t) => {
  const fixture = await heatingServer(t);
  const first = await heating(fixture.context);
  const second = await heating({ ...fixture.context, state: first.state });
  assert.deepEqual(second.events, []);
  fixture.values.boilerTemp = "60";
  const equal = await heating({ ...fixture.context, state: second.state });
  assert.deepEqual(equal.events, []);
  fixture.values.boilerTemp = "59";
  const again = await heating({ ...fixture.context, state: equal.state });
  assert.equal(again.events.length, 1);
  assert.equal(again.events[0].payload.value, 59);
});

test("Fröling prefers the assigned token and shares one password fallback across expired-token requests", async (t) => {
  let logins = 0,
    requests = 0,
    expired = false;
  t.mock.method(globalThis, "fetch", async (url, options) => {
    requests++;
    if (new URL(url).pathname.endsWith("/login")) {
      logins++;
      return new Response("{}", {
        headers: { Authorization: "Bearer renewed-token" },
      });
    }
    const auth = options.headers.Authorization;
    assert.ok(["Bearer assigned-token", "Bearer renewed-token"].includes(auth));
    if (expired && auth === "Bearer assigned-token")
      return new Response("private-body", { status: 401 });
    return Response.json({ stateView: [{ name: "temperature", value: "55" }] });
  });
  const ctx = {
    config: {
      userId: "user",
      facilityId: "facility",
      accessTokenSecret: "token",
    },
    secrets: { ...credentials, token: "Bearer assigned-token" },
    signal: signal(),
  };
  await heating(ctx);
  assert.equal(logins, 0);
  assert.equal(requests, 3);
  expired = true;
  const result = await heating(ctx);
  assert.equal(logins, 1);
  assert.doesNotMatch(
    JSON.stringify(result),
    /assigned-token|renewed-token|fixture-password/,
  );
  await assert.rejects(
    heating({ ...ctx, secrets: { token: "Bearer assigned-token" } }),
    { code: "authentication_required" },
  );
  assert.equal(logins, 1);
});

test("Fröling discovery returns facility and component IDs without exposing its JWT or unrelated account fields", async (t) => {
  const jwt =
    "header." +
    Buffer.from(
      JSON.stringify({ userId: 123, privateField: "hidden" }),
    ).toString("base64url") +
    ".signature";
  const calls = [];
  let invalidFacility = false;
  t.mock.method(globalThis, "fetch", async (url, options) => {
    if (invalidFacility) return Response.json([{ facilityId: "../escape" }]);
    const path = new URL(url).pathname;
    calls.push(path);
    if (path.endsWith("/login"))
      return new Response("{}", { headers: { Authorization: jwt } });
    assert.equal(options.headers.Authorization, jwt);
    if (path.endsWith("/facility"))
      return Response.json([{ facilityId: 7, privateAccountField: "hidden" }]);
    assert.equal(path, "/fcs/v1.0/resources/user/123/facility/7/componentList");
    return Response.json([
      { componentId: "1_100" },
      { componentId: "400_4100" },
    ]);
  });
  const result = await heating({
    config: { discoveryOnly: true },
    secrets: credentials,
    signal: signal(),
  });
  assert.equal(result.data.userId, "123");
  assert.deepEqual(result.data.facilities, [
    { facilityId: "7", componentIds: ["1_100", "400_4100"] },
  ]);
  assert.equal(calls.length, 3);
  assert.doesNotMatch(
    JSON.stringify(result),
    /signature|hidden|fixture-password/,
  );
  invalidFacility = true;
  await assert.rejects(
    heating({
      config: {
        discoveryOnly: true,
        userId: "123",
        accessTokenSecret: "token",
      },
      secrets: { token: jwt },
      signal: signal(),
    }),
    { code: "provider_unavailable" },
  );
});

test("Fröling rejects login/partial-read failures and configuration mistakes without response-body leaks", async (t) => {
  const fixture = await heatingServer(t);
  fixture.failures["400_4100"] = true;
  await assert.rejects(
    heating(fixture.context),
    (error) =>
      /HTTP 503/.test(error.message) && !error.message.includes("secret"),
  );
  fixture.removeAuthorization();
  await assert.rejects(heating(fixture.context), /no authorization header/);
  const count = fixture.requests.length;
  await assert.rejects(
    heating({ ...fixture.context, secrets: {} }),
    /secrets are required/,
  );
  await assert.rejects(
    heating({
      ...fixture.context,
      config: { ...fixture.context.config, facilityId: "" },
    }),
    /facilityId/,
  );
  assert.equal(fixture.requests.length, count);
});

test("Fröling honors cancellation before making a network request", async (t) => {
  let calls = 0;
  t.mock.method(globalThis, "fetch", async () => {
    calls++;
    throw Error("Unexpected request");
  });
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    heating({
      config: { userId: "user", facilityId: "facility" },
      secrets: credentials,
      signal: controller.signal,
    }),
    { name: "AbortError" },
  );
  assert.equal(calls, 0);
});

function appliance(device, input, state = null) {
  return laundry({ config: { device }, input, state, signal: signal() });
}

test("washer establishes a baseline, emits completion once and persists the latest push", async () => {
  const first = await appliance("washer", {
    cycle_st: 11,
    tte_showed: 0,
    timestamp: 10,
  });
  assert.deepEqual(first.events, []);
  const running = await appliance(
    "washer",
    { cycle_st: 1, tte_showed: 120, timestamp: 11 },
    first.state,
  );
  assert.equal(running.data.running, true);
  assert.equal(running.events[0].name, "laundry.status.changed");
  const finished = await appliance(
    "washer",
    { cycle_st: 11, tte_showed: 0, timestamp: 12 },
    running.state,
  );
  assert.deepEqual(
    finished.events.map((e) => e.name),
    ["laundry.finished", "laundry.status.changed"],
  );
  assert.equal(finished.events[0].payload.device, "washer");
  assert.deepEqual(
    (await appliance("washer", finished.data.payload, finished.state)).events,
    [],
  );
  const older = await appliance(
    "washer",
    { cycle_st: 1, tte_showed: 300, timestamp: 9 },
    finished.state,
  );
  assert.equal(older.data.cycleState, 11);
  assert.equal(older.data.ignoredOlderInput, true);
  assert.deepEqual(older.state, finished.state);
  assert.deepEqual(older.events, []);
});

test("dryer completion requires a known positive prior duration; unknown sentinels never finish", async () => {
  const payload = (seconds) => ({
    cycle_timer: 1,
    step_timer: 2,
    tte_showed: seconds,
  });
  const unknown = await appliance("dryer", payload(65535));
  assert.equal(unknown.data.remainingSeconds, null);
  assert.equal(unknown.data.running, false);
  assert.deepEqual(unknown.events, []);
  const idle = await appliance("dryer", payload(0), unknown.state);
  assert.ok(idle.events.every((e) => e.name !== "laundry.finished"));
  const running = await appliance("dryer", payload(45), idle.state);
  const finished = await appliance("dryer", payload(0), running.state);
  assert.equal(
    finished.events.filter((e) => e.name === "laundry.finished").length,
    1,
  );
  assert.deepEqual(
    (await appliance("dryer", payload(0), finished.state)).events,
    [],
  );
});

test("laundry rejects malformed pushes and keeps washer completion when the optional timer is absent", async () => {
  await assert.rejects(appliance("washer", { cycle_st: "11" }), /cycle_st/);
  await assert.rejects(
    appliance("dryer", { cycle_timer: 1, tte_showed: 0 }),
    /step_timer/,
  );
  await assert.rejects(
    appliance("washer", { cycle_st: 1, tte_showed: -1 }),
    /tte_showed/,
  );
  await assert.rejects(
    appliance("washer", { cycle_st: 1, timestamp: "invalid" }),
    /timestamp/,
  );
  const running = await appliance("washer", { cycle_st: 1, timestamp: 10 });
  const finished = await appliance(
    "washer",
    { cycle_st: 11, timestamp: 11 },
    running.state,
  );
  assert.equal(finished.events[0].name, "laundry.finished");
  assert.equal(finished.data.remainingSeconds, null);
});

const teslaContext = () => ({
  config: {
    orderUrl: "https://www.tesla.com/en_US/teslaaccount/order/RN123456789",
  },
  secrets: { tesla_session_cookie: "fixture_session=private-fixture-cookie" },
  state: null,
  signal: signal(),
});
const htmlResponse = (html) =>
  new Response(html, {
    headers: { "Content-Type": "text/html; charset=utf-8" },
  });

test("Tesla extracts actual delivery text, ignores script/hidden decoys and emits only changed windows", async (t) => {
  const context = teslaContext();
  t.mock.method(globalThis, "fetch", async (url, options) => {
    assert.equal(String(url), context.config.orderUrl);
    assert.equal(options.headers.Cookie, context.secrets.tesla_session_cookie);
    assert.equal(options.signal, context.signal);
    assert.equal(options.redirect, "manual");
    return htmlResponse(
      '<script>Estimated Delivery: January 1 - 2</script><div hidden>Estimated Delivery: February 2 - 3</div><div aria-hidden="true">Estimated Delivery: March 3 - 4</div><span>Estimated Delivery:</span><span>September&nbsp;19 &ndash; October 3, 2026</span>',
    );
  });
  const first = await tesla(context);
  assert.equal(first.data.estimatedDelivery, "September 19 - October 3, 2026");
  assert.deepEqual(first.events, []);
  const changed = await tesla({
    ...context,
    state: { estimatedDelivery: "September 1 - 2" },
  });
  assert.equal(changed.events[0].name, "tesla.delivery.changed");
  assert.deepEqual(
    (await tesla({ ...context, state: first.state })).events,
    [],
  );
  assert.doesNotMatch(
    JSON.stringify(changed),
    /private-fixture-cookie|fixture_session/,
  );
});

test("Tesla supports German day/month order without relying on browser execution", async (t) => {
  t.mock.method(globalThis, "fetch", async () =>
    htmlResponse(
      "<main>Voraussichtliche Lieferung: 19. September – 3. Oktober 2026</main>",
    ),
  );
  assert.equal(
    (await tesla(teslaContext())).data.estimatedDelivery,
    "19. September - 3. Oktober 2026",
  );
});

test("Tesla follows locale redirects only for the configured RN order", async (t) => {
  const context = teslaContext();
  context.state = { estimatedDelivery: "September 1 - 2" };
  const target = "https://www.tesla.com/de_DE/teslaaccount/order/RN123456789/";
  const calls = [];
  t.mock.method(globalThis, "fetch", async (url, options) => {
    calls.push(String(url));
    assert.equal(options.headers.Cookie, context.secrets.tesla_session_cookie);
    assert.equal(options.redirect, "manual");
    if (calls.length === 1) {
      return new Response(null, {
        status: 302,
        headers: { Location: target },
      });
    }
    return htmlResponse(
      "<main>Voraussichtliche Lieferung: 19. September – 3. Oktober 2026</main>",
    );
  });
  const result = await tesla(context);
  assert.deepEqual(calls, [context.config.orderUrl, target]);
  assert.equal(
    result.data.estimatedDelivery,
    "19. September - 3. Oktober 2026",
  );
  assert.equal(result.events[0].name, "tesla.delivery.changed");
  assert.equal(
    result.events[0].payload.previous,
    context.state.estimatedDelivery,
  );
});

test("Tesla refuses dashboard, other-order and malformed redirects before collecting their delivery", async (t) => {
  const context = teslaContext();
  context.state = { estimatedDelivery: "September 1 - 2" };
  for (const target of [
    "/de_DE/teslaaccount",
    "/en_US/teslaaccount/order/RN987654321",
    "https://[",
  ]) {
    let calls = 0;
    t.mock.method(globalThis, "fetch", async () => {
      calls++;
      if (calls === 1) {
        return new Response(null, {
          status: 302,
          headers: { Location: target },
        });
      }
      return htmlResponse("<main>Estimated Delivery: December 1 - 2</main>");
    });
    await assert.rejects(tesla(context), { code: "provider_unavailable" });
    assert.equal(calls, 1);
    assert.deepEqual(context.state, { estimatedDelivery: "September 1 - 2" });
  }
});

test("Tesla stops at expired sessions and fails explicitly for client-rendered account shells", async (t) => {
  let calls = 0;
  t.mock.method(globalThis, "fetch", async () => {
    calls++;
    return new Response(null, {
      status: 302,
      headers: { Location: "https://auth.tesla.com/oauth2/v1/authorize" },
    });
  });
  await assert.rejects(tesla(teslaContext()), {
    code: "authentication_required",
  });
  assert.equal(calls, 1);
  t.mock.method(globalThis, "fetch", async () =>
    htmlResponse('<form><input type="password"></form>'),
  );
  await assert.rejects(tesla(teslaContext()), {
    code: "authentication_required",
  });
  t.mock.method(globalThis, "fetch", async () =>
    htmlResponse(
      '<main id="app"></main><script>window.delivery="September 1 - 2";</script>',
    ),
  );
  await assert.rejects(tesla(teslaContext()), {
    code: "provider_unavailable",
  });
});

test("Tesla rejects credential forwarding, missing cookies, failed HTTP and non-HTML responses", async (t) => {
  let calls = 0;
  t.mock.method(globalThis, "fetch", async () => {
    calls++;
    return new Response("secret response", { status: 403 });
  });
  const context = teslaContext();
  await assert.rejects(
    tesla({
      ...context,
      config: {
        orderUrl: "https://unrelated.example/teslaaccount/order/RN123456789",
      },
    }),
    { code: "configuration_invalid" },
  );
  await assert.rejects(tesla({ ...context, secrets: {} }), {
    code: "authentication_required",
  });
  assert.equal(calls, 0);
  await assert.rejects(
    tesla(context),
    (error) =>
      error.code === "provider_unavailable" &&
      !error.message.includes("secret"),
  );
  t.mock.method(
    globalThis,
    "fetch",
    async () =>
      new Response("{}", { headers: { "Content-Type": "application/json" } }),
  );
  await assert.rejects(tesla(context), { code: "provider_unavailable" });
  t.mock.method(globalThis, "fetch", async () => {
    throw new Error(context.secrets.tesla_session_cookie);
  });
  await assert.rejects(
    tesla(context),
    (error) =>
      error.code === "provider_unavailable" &&
      !error.message.includes(context.secrets.tesla_session_cookie),
  );
});

test("home templates are disabled and materializable, with explicitly token-free appliance input", async () => {
  for (const name of ["froeling", "washer", "dryer", "tesla-delivery"]) {
    const template = JSON.parse(
      await readFile(new URL(`${name}.task.json`, home), "utf8"),
    );
    assert.equal(template.enabled, false);
    assert.match(template.script, /^@file:[a-z-]+\.mjs$/);
    const source = await readFile(
      new URL(template.script.slice(6), home),
      "utf8",
    );
    assert.match(source, /export default async/);
    assert.equal(template.retention.maximumCount, 1000);
    if (["washer", "dryer"].includes(name)) {
      assert.equal(template.intervalSeconds, 0);
      assert.equal(template.allowUnauthenticatedInput, true);
      assert.equal(template.inputSecretName, undefined);
      assert.deepEqual(template.secretNames, []);
    }
  }
});
