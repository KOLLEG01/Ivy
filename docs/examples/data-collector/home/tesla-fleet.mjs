import { createHash, randomBytes, randomUUID } from "node:crypto";
import { chmod, mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { dirname, join } from "node:path";

const tokenUrl = "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token";
const failure = (code, message) => Object.assign(new Error(message), { code });
const tokenValue = (value) =>
  typeof value === "string" && value.length > 0 && !/[\r\n]/.test(value);

export function fleetBaseUrl(region) {
  if (!["eu", "na"].includes(region))
    throw failure(
      "configuration_invalid",
      "Choose the Fleet API region eu or na.",
    );
  return `https://fleet-api.prd.${region}.vn.cloud.tesla.com`;
}

async function requestJson(url, options, signal, authentication = false) {
  signal?.throwIfAborted();
  let response;
  try {
    response = await fetch(url, {
      ...options,
      redirect: "error",
      signal: signal
        ? AbortSignal.any([signal, AbortSignal.timeout(15000)])
        : AbortSignal.timeout(15000),
    });
  } catch {
    signal?.throwIfAborted();
    throw failure("provider_unavailable", "Tesla API request failed.");
  }
  if (!response.ok) {
    await response.body?.cancel();
    const code =
      response.status === 401 || (authentication && response.status === 400)
        ? "authentication_required"
        : response.status === 403
          ? "interaction_required"
          : "provider_unavailable";
    throw failure(code, `Tesla API request failed (HTTP ${response.status}).`);
  }
  try {
    const text = await response.text();
    if (Buffer.byteLength(text) > (authentication ? 65536 : 750000))
      throw new Error();
    return JSON.parse(text);
  } catch {
    signal?.throwIfAborted();
    throw failure(
      "provider_unavailable",
      "Tesla returned an unsupported API response.",
    );
  }
}

function tokens(value) {
  if (
    !tokenValue(value?.access_token) ||
    !tokenValue(value?.refresh_token) ||
    (value.token_type != null &&
      (typeof value.token_type !== "string" ||
        value.token_type.toLowerCase() !== "bearer")) ||
    !Number.isSafeInteger(value.expires_in) ||
    value.expires_in <= 0 ||
    value.expires_in > 2592000
  )
    throw failure(
      "provider_unavailable",
      "Tesla returned an unsupported token response.",
    );
  return {
    accessToken: value.access_token,
    refreshToken: value.refresh_token,
    expiresAt: Date.now() + value.expires_in * 1000,
  };
}

export function createAuthorizationRequest({
  clientId,
  redirectUri,
  region = "eu",
}) {
  fleetBaseUrl(region);
  if (!tokenValue(clientId))
    throw failure(
      "configuration_invalid",
      "A Tesla application client ID is required.",
    );
  let redirect;
  try {
    redirect = new URL(redirectUri);
  } catch {
    throw failure(
      "configuration_invalid",
      "A registered HTTPS redirect URI is required.",
    );
  }
  if (
    redirect.protocol !== "https:" ||
    redirect.username ||
    redirect.password ||
    redirect.search ||
    redirect.hash
  )
    throw failure(
      "configuration_invalid",
      "Use a registered HTTPS redirect URI without credentials or query parameters.",
    );
  const state = randomBytes(32).toString("base64url");
  const url = new URL("https://auth.tesla.com/oauth2/v3/authorize");
  url.search = new URLSearchParams({
    client_id: clientId,
    redirect_uri: redirectUri,
    response_type: "code",
    scope: "openid offline_access user_data vehicle_device_data",
    state,
    nonce: randomBytes(32).toString("base64url"),
    require_requested_scopes: "true",
  }).toString();
  return { clientId, redirectUri, region, state, authorizationUrl: url.href };
}

export async function exchangeAuthorizationCode(
  request,
  callbackUrl,
  clientSecret,
  signal,
) {
  fleetBaseUrl(request?.region);
  if (
    !tokenValue(request?.clientId) ||
    !tokenValue(clientSecret) ||
    !tokenValue(request?.state)
  )
    throw failure(
      "configuration_invalid",
      "The authorization request and application credentials are required.",
    );
  let callback, redirect;
  try {
    callback = new URL(callbackUrl.trim());
    redirect = new URL(request.redirectUri);
  } catch {
    throw failure(
      "configuration_invalid",
      "The complete Tesla redirect URL is required.",
    );
  }
  if (
    callback.origin !== redirect.origin ||
    callback.pathname !== redirect.pathname ||
    callback.username ||
    callback.password ||
    callback.hash ||
    callback.searchParams.getAll("state").length !== 1 ||
    callback.searchParams.get("state") !== request.state ||
    callback.searchParams.getAll("code").length !== 1
  )
    throw failure(
      "authentication_required",
      "The Tesla callback does not match this authorization request.",
    );
  const code = callback.searchParams.get("code");
  if (!tokenValue(code) || callback.searchParams.has("error"))
    throw failure(
      "authentication_required",
      "Tesla authorization was not completed.",
    );
  return tokens(
    await requestJson(
      tokenUrl,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/x-www-form-urlencoded",
          Accept: "application/json",
        },
        body: new URLSearchParams({
          grant_type: "authorization_code",
          client_id: request.clientId,
          client_secret: clientSecret,
          code,
          redirect_uri: request.redirectUri,
          audience: fleetBaseUrl(request.region),
        }),
      },
      signal,
      true,
    ),
  );
}

async function saveAuthentication(file, value) {
  const temporary = `${file}.${randomUUID()}.tmp`;
  let handle;
  try {
    await mkdir(dirname(file), { recursive: true, mode: 0o700 });
    await chmod(dirname(file), 0o700);
    handle = await open(temporary, "wx", 0o600);
    await handle.writeFile(JSON.stringify(value));
    await handle.sync();
    await handle.close();
    handle = null;
    await rename(temporary, file);
  } catch {
    throw failure(
      "configuration_invalid",
      "The private Tesla authentication cache could not be saved.",
    );
  } finally {
    await handle?.close().catch(() => undefined);
    await rm(temporary, { force: true }).catch(() => undefined);
  }
}

async function authentication(config, secrets, signal) {
  const clientId = secrets[config.clientIdSecret ?? "tesla_client_id"];
  const initialRefreshToken =
    secrets[config.refreshTokenSecret ?? "tesla_refresh_token"];
  if (!tokenValue(clientId) || !tokenValue(initialRefreshToken))
    throw failure(
      "authentication_required",
      "Assign the Tesla client ID and an authorized third-party refresh token.",
    );
  const fingerprint = createHash("sha256")
    .update(
      JSON.stringify({
        clientId,
        initialRefreshToken,
        region: config.region,
      }),
    )
    .digest("hex");
  const file = join(process.cwd(), ".auth", "tesla-fleet.json");
  let auth;
  try {
    const cached = JSON.parse(await readFile(file, "utf8"));
    if (cached.version !== 1 || !/^[a-f0-9]{64}$/.test(cached.fingerprint))
      throw new Error();
    if (cached.fingerprint === fingerprint) {
      if (
        !tokenValue(cached.accessToken) ||
        !tokenValue(cached.refreshToken) ||
        !Number.isSafeInteger(cached.expiresAt)
      )
        throw new Error();
      auth = cached;
    }
  } catch (error) {
    if (error.code !== "ENOENT")
      throw failure(
        "configuration_invalid",
        "The private Tesla authentication cache is invalid or unreadable.",
      );
  }
  const refresh = async () => {
    const renewed = tokens(
      await requestJson(
        tokenUrl,
        {
          method: "POST",
          headers: {
            "Content-Type": "application/x-www-form-urlencoded",
            Accept: "application/json",
          },
          body: new URLSearchParams({
            grant_type: "refresh_token",
            client_id: clientId,
            refresh_token: auth?.refreshToken ?? initialRefreshToken,
          }),
        },
        signal,
        true,
      ),
    );
    auth = { version: 1, fingerprint, ...renewed };
    // Commit rotating credentials before any later data request can fail.
    await saveAuthentication(file, auth);
  };
  if (!auth || auth.expiresAt <= Date.now() + 60000) await refresh();
  let retried = false;
  return async (path) => {
    const send = () =>
      requestJson(
        fleetBaseUrl(config.region) + path,
        {
          headers: {
            Authorization: `Bearer ${auth.accessToken}`,
            Accept: "application/json",
          },
        },
        signal,
      );
    try {
      return await send();
    } catch (error) {
      if (error.code !== "authentication_required" || retried) throw error;
      retried = true;
      await refresh();
      return send();
    }
  };
}

function pointer(value, path) {
  if (
    typeof path !== "string" ||
    !path.startsWith("/") ||
    /~(?![01])/.test(path)
  )
    throw failure(
      "configuration_invalid",
      "API field mappings must be JSON pointers.",
    );
  for (const segment of path.slice(1).split("/")) {
    const key = segment.replace(/~1/g, "/").replace(/~0/g, "~");
    if (
      value === null ||
      typeof value !== "object" ||
      !Object.hasOwn(value, key)
    )
      return undefined;
    value = value[key];
  }
  return value;
}

const canonical = (value) =>
  JSON.stringify(value, function (_key, item) {
    return item && typeof item === "object" && !Array.isArray(item)
      ? Object.fromEntries(
          Object.keys(item)
            .sort()
            .map((key) => [key, item[key]]),
        )
      : item;
  });

export default async function collect({ config, secrets, state, signal }) {
  fleetBaseUrl(config.region);
  if (typeof config.expectedEmail !== "string" || !config.expectedEmail.trim())
    throw failure(
      "configuration_invalid",
      "Set expectedEmail to the authorized Tesla account email.",
    );
  // Validate mappings before rotating a token or contacting the provider.
  if (config.deliveryPointer != null) pointer({}, config.deliveryPointer);
  if (config.orderSelector != null) {
    pointer({}, config.orderSelector.path);
    if (
      typeof config.orderSelector.value !== "string" ||
      !config.orderSelector.value
    )
      throw failure(
        "configuration_invalid",
        "An order selector value is required.",
      );
  }
  const api = await authentication(config, secrets, signal);
  const account = (await api("/api/1/users/me"))?.response;
  if (typeof account?.email !== "string")
    throw failure(
      "provider_unavailable",
      "Tesla did not return a verifiable account identity.",
    );
  if (
    account.email.trim().toLowerCase() !==
    config.expectedEmail.trim().toLowerCase()
  )
    throw failure(
      "configuration_invalid",
      "The Tesla authorization belongs to a different account.",
    );
  const orders = (await api("/api/1/users/orders"))?.response;
  if (
    !Array.isArray(orders) ||
    orders.some(
      (order) => !order || typeof order !== "object" || Array.isArray(order),
    )
  )
    throw failure(
      "provider_unavailable",
      "Tesla did not return an active-order list.",
    );
  const selected = config.orderSelector
    ? orders.filter(
        (order) =>
          pointer(order, config.orderSelector.path) ===
          config.orderSelector.value,
      )
    : orders;
  let delivery = { status: "not_configured", value: null };
  if (config.deliveryPointer != null) {
    if (selected.length > 1)
      throw failure(
        "configuration_invalid",
        "Select exactly one order before mapping its delivery field.",
      );
    const value = selected.length
      ? pointer(selected[0], config.deliveryPointer)
      : undefined;
    delivery = {
      status: !selected.length
        ? "order_not_found"
        : value == null
          ? "field_missing"
          : "available",
      value: value ?? null,
    };
  }
  const observedAt = new Date().toISOString();
  const ordersHash = createHash("sha256")
    .update(JSON.stringify(orders.map(canonical).sort()))
    .digest("hex");
  const estimatedDelivery =
    typeof delivery.value === "string" ? delivery.value : null;
  const events = [];
  if (typeof state?.ordersHash === "string" && state.ordersHash !== ordersHash)
    events.push({
      name: "tesla.orders.changed",
      payload: { orderCount: orders.length, observedAt },
    });
  if (
    typeof state?.estimatedDelivery === "string" &&
    estimatedDelivery !== null &&
    state.estimatedDelivery !== estimatedDelivery
  )
    events.push({
      name: "tesla.delivery.changed",
      payload: {
        previous: state.estimatedDelivery,
        estimatedDelivery,
        observedAt,
      },
    });
  return {
    data: {
      source: "tesla-fleet-api",
      observedAt,
      account: { email: account.email },
      orders,
      delivery,
      estimatedDelivery,
    },
    state: { ordersHash, estimatedDelivery },
    events,
  };
}
