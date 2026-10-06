// Fröling Connect's web protocol is undocumented; verify it before enabling.
export default async function collect({ config, secrets, state, signal }) {
  const username = secrets[config.usernameSecret ?? "froeling_username"];
  const password = secrets[config.passwordSecret ?? "froeling_password"];
  const token = config.accessTokenSecret
    ? secrets[config.accessTokenSecret]
    : null;
  const hasPassword =
    typeof username === "string" &&
    username &&
    typeof password === "string" &&
    password;
  if (!hasPassword && !(typeof token === "string" && token.trim())) {
    throw failure(
      "configuration_invalid",
      "Fröling token or username and password secrets are required",
    );
  }
  if (
    token != null &&
    (typeof token !== "string" || !token.trim() || /[\r\n]/.test(token))
  )
    throw failure("configuration_invalid", "Invalid Fröling token secret");
  for (const key of config.discoveryOnly ? [] : ["userId", "facilityId"]) {
    if (
      typeof config[key] !== "string" ||
      !/^[A-Za-z0-9_-]+$/.test(config[key])
    ) {
      throw failure(
        "configuration_invalid",
        `Fröling ${key} must be configured`,
      );
    }
  }
  const base = new URL(config.baseUrl ?? "https://connect-api.froeling.com");
  if (
    !["http:", "https:"].includes(base.protocol) ||
    base.username ||
    base.password ||
    base.search ||
    base.hash
  ) {
    throw failure("configuration_invalid", "Invalid Fröling baseUrl");
  }
  const components = config.components ?? {
    boiler: "1_100",
    buffer: "400_4100",
    circuit: "300_3110",
  };
  const entries = Object.entries(components);
  if (
    !entries.length ||
    entries.some(
      ([name, id]) =>
        !/^[a-z][a-z0-9_-]*$/i.test(name) ||
        typeof id !== "string" ||
        !/^[A-Za-z0-9_-]+$/.test(id),
    )
  ) {
    throw failure(
      "configuration_invalid",
      "Fröling components must map names to component IDs",
    );
  }
  const thresholds = config.thresholds ?? [];
  if (
    !Array.isArray(thresholds) ||
    thresholds.some(
      (t) =>
        !t ||
        typeof t.component !== "string" ||
        typeof t.metric !== "string" ||
        !Number.isFinite(t.below),
    )
  ) {
    throw failure("configuration_invalid", "Invalid Fröling thresholds");
  }
  async function request(path, options = {}) {
    signal?.throwIfAborted();
    let response;
    try {
      response = await fetch(new URL(path, base), {
        ...options,
        signal,
        redirect: "error",
      });
    } catch {
      signal?.throwIfAborted();
      throw failure("provider_unavailable", "Fröling request failed");
    }
    if (!response.ok) {
      await response.body?.cancel();
      throw Object.assign(
        failure(
          response.status === 401
            ? "authentication_required"
            : "provider_unavailable",
          `Fröling request failed (HTTP ${response.status})`,
        ),
        { status: response.status },
      );
    }
    return response;
  }
  async function login() {
    const response = await request("/connect/v1.0/resources/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ osType: "web", username, password }),
    });
    const value = response.headers.get("Authorization");
    await response.body?.cancel();
    if (!value)
      throw failure(
        "authentication_required",
        "Fröling login returned no authorization header",
      );
    return value;
  }
  let authorization = token || (await login());
  let renewed;
  async function get(path) {
    let response;
    try {
      response = await request(path, {
        headers: { Authorization: authorization },
      });
    } catch (error) {
      if (error.status !== 401 || !token || !hasPassword) throw error;
      authorization = await (renewed ??= login());
      response = await request(path, {
        headers: { Authorization: authorization },
      });
    }
    try {
      return await response.json();
    } catch {
      signal?.throwIfAborted();
      throw failure("provider_unavailable", "Fröling returned invalid JSON");
    }
  }
  if (config.discoveryOnly) {
    let userId = config.userId;
    if (!userId) {
      try {
        userId = String(
          JSON.parse(
            Buffer.from(
              authorization.replace(/^Bearer /i, "").split(".")[1],
              "base64url",
            ),
          ).userId ?? "",
        );
      } catch {
        /* Explicit userId is needed for non-JWT tokens. */
      }
    }
    if (typeof userId !== "string" || !/^[A-Za-z0-9_-]+$/.test(userId))
      throw failure(
        "configuration_invalid",
        "Fröling discovery needs userId or a token containing userId",
      );
    const facilities = await get(
      `/connect/v1.0/resources/service/user/${userId}/facility`,
    );
    if (!Array.isArray(facilities))
      throw failure(
        "provider_unavailable",
        "Fröling returned an invalid facility list",
      );
    const inventory = [];
    for (const facility of facilities) {
      const facilityId = String(facility?.facilityId ?? "");
      if (!/^[A-Za-z0-9_-]+$/.test(facilityId))
        throw failure(
          "provider_unavailable",
          "Fröling returned an invalid facility ID",
        );
      const components = await get(
        `/fcs/v1.0/resources/user/${userId}/facility/${facilityId}/componentList`,
      );
      if (
        !Array.isArray(components) ||
        components.some(
          (c) => !/^[A-Za-z0-9_-]+$/.test(String(c?.componentId ?? "")),
        )
      )
        throw failure(
          "provider_unavailable",
          "Fröling returned an invalid component list",
        );
      inventory.push({
        facilityId,
        componentIds: components.map((c) => String(c.componentId)),
      });
    }
    return {
      data: {
        observedAt: new Date().toISOString(),
        userId,
        facilities: inventory,
      },
    };
  }
  const observations = await Promise.all(
    entries.map(async ([name, id]) => {
      const raw = await get(
        `/fcs/v1.0/resources/user/${config.userId}/facility/${config.facilityId}/component/${id}`,
      );
      const picture = raw?.topView?.pictureParams;
      const records = [
        picture?.state,
        picture?.bufferPumpControl,
        ...(Array.isArray(raw?.stateView) ? raw.stateView : []),
      ];
      const values = Object.fromEntries(
        records
          .filter(
            (r) =>
              r &&
              typeof r.name === "string" &&
              r.name &&
              ["string", "number"].includes(typeof r.value),
          )
          .map((r) => {
            const number =
              typeof r.value === "number"
                ? r.value
                : Number.parseFloat(r.value.trim().replace(",", "."));
            return [r.name, Number.isFinite(number) ? number : r.value];
          }),
      );
      if (!Object.keys(values).length)
        throw failure(
          "provider_unavailable",
          "Fröling component returned no telemetry",
        );
      return [name, { values }];
    }),
  );
  const observedAt = new Date().toISOString();
  const data = { observedAt, components: Object.fromEntries(observations) };
  const previous = state?.below ?? {};
  const below = {};
  const events = [];
  for (const threshold of thresholds) {
    const key = JSON.stringify([
      threshold.component,
      threshold.metric,
      threshold.below,
    ]);
    const value =
      data.components[threshold.component]?.values[threshold.metric];
    if (typeof value !== "number" || !Number.isFinite(value)) {
      if (typeof previous[key] === "boolean") below[key] = previous[key];
      continue;
    }
    below[key] = value < threshold.below;
    if (below[key] && previous[key] !== true) {
      events.push({
        name: "heating.threshold.crossed",
        payload: {
          component: threshold.component,
          metric: threshold.metric,
          value,
          below: threshold.below,
          observedAt,
        },
      });
    }
  }
  return { data, state: { below }, events };
}

function failure(code, message) {
  return Object.assign(new Error(message), { code });
}
