import { parse } from "parse5";

// Reads authenticated server HTML only. A client-rendered account shell is an error.
export default async function collect({ config, secrets, state, signal }) {
  const cookie = secrets[config.cookieSecret ?? "tesla_session_cookie"];
  if (typeof cookie !== "string" || !cookie || /[\r\n]/.test(cookie)) {
    throw failure(
      "authentication_required",
      "An assigned Tesla account session cookie is required",
    );
  }
  let url;
  try {
    url = new URL(config.orderUrl);
  } catch {
    throw failure("configuration_invalid", "A Tesla orderUrl is required");
  }
  const orderPath =
    /^\/(?:[a-z]{2}_[a-z]{2}\/)?teslaaccount\/order\/(RN[A-Z0-9-]+)\/?$/i;
  const orderNumber = orderPath.exec(url.pathname)?.[1].toUpperCase();
  if (
    url.protocol !== "https:" ||
    !["www.tesla.com", "tesla.com"].includes(url.hostname) ||
    url.username ||
    url.password ||
    url.hash ||
    !orderNumber
  ) {
    throw failure(
      "configuration_invalid",
      "orderUrl must identify a Tesla account order page",
    );
  }
  let response;
  for (let redirects = 0; redirects <= 3; redirects++) {
    signal?.throwIfAborted();
    try {
      response = await fetch(url, {
        headers: { Cookie: cookie, Accept: "text/html" },
        signal,
        redirect: "manual",
      });
    } catch {
      signal?.throwIfAborted();
      throw failure("provider_unavailable", "Tesla account request failed");
    }
    if (response.status < 300 || response.status >= 400) break;
    const location = response.headers.get("location");
    await response.body?.cancel();
    let next;
    try {
      next = location ? new URL(location, url) : null;
    } catch {
      throw failure(
        "provider_unavailable",
        "Tesla returned an invalid redirect",
      );
    }
    if (
      !next ||
      next.origin !== url.origin ||
      next.username ||
      next.password ||
      next.hash
    ) {
      throw failure(
        "authentication_required",
        "Tesla account session must be renewed",
      );
    }
    if (orderPath.exec(next.pathname)?.[1].toUpperCase() !== orderNumber) {
      throw failure(
        "provider_unavailable",
        "Tesla redirected away from the configured order",
      );
    }
    if (redirects === 3)
      throw failure(
        "provider_unavailable",
        "Tesla account returned too many redirects",
      );
    url = next;
  }
  if (response.status === 401)
    throw failure(
      "authentication_required",
      "Tesla account session must be renewed",
    );
  if (!response.ok)
    throw failure(
      "provider_unavailable",
      `Tesla account request failed (HTTP ${response.status})`,
    );
  if (!/text\/html/i.test(response.headers.get("content-type") ?? "")) {
    throw failure("provider_unavailable", "Tesla account did not return HTML");
  }
  const html = await response.text();
  if (Buffer.byteLength(html) > 2 * 1024 * 1024)
    throw failure("provider_unavailable", "Tesla account HTML is too large");
  const document = parse(html);
  const chunks = [];
  let passwordInput = false;
  function walk(node) {
    const attrs = Object.fromEntries(
      (node.attrs ?? []).map((a) => [a.name, a.value]),
    );
    if (node.tagName === "input" && attrs.type?.toLowerCase() === "password")
      passwordInput = true;
    if (
      ["script", "style", "template", "noscript"].includes(node.tagName) ||
      "hidden" in attrs ||
      attrs["aria-hidden"] === "true" ||
      /(?:display\s*:\s*none|visibility\s*:\s*hidden)/i.test(attrs.style ?? "")
    )
      return;
    if (node.nodeName === "#text") chunks.push(node.value);
    for (const child of node.childNodes ?? []) walk(child);
  }
  walk(document);
  if (passwordInput)
    throw failure(
      "authentication_required",
      "Tesla account session must be renewed",
    );
  const text = chunks.join(" ").replace(/\s+/g, " ");
  const pattern =
    /(?:\b(?:Est\.?|Estimated)\s+Delivery|Voraussichtliche\s+Lieferung)\s*:\s*(\p{L}[\p{L}.]*\s+\d{1,2}(?:,\s*\d{4})?\s*[-–—]\s*(?:\p{L}[\p{L}.]*\s+)?\d{1,2}(?:,\s*\d{4})?)/iu;
  const germanDate =
    /Voraussichtliche\s+Lieferung\s*:\s*(\d{1,2}\.\s*\p{L}[\p{L}.]*(?:\s+\d{4})?\s*[-–—]\s*\d{1,2}\.\s*\p{L}[\p{L}.]*(?:\s+\d{4})?)/iu;
  const match = text.match(pattern) ?? text.match(germanDate);
  if (!match) {
    throw failure(
      "provider_unavailable",
      "Tesla returned no delivery window in server HTML; a verified backend order API is required for client-rendered pages",
    );
  }
  const estimatedDelivery = match[1].replace(/\s*[-–—]\s*/g, " - ");
  const observedAt = new Date().toISOString();
  const data = { estimatedDelivery, observedAt, source: "tesla-account-html" };
  const events =
    typeof state?.estimatedDelivery === "string" &&
    state.estimatedDelivery !== estimatedDelivery
      ? [
          {
            name: "tesla.delivery.changed",
            payload: {
              previous: state.estimatedDelivery,
              estimatedDelivery,
              observedAt,
            },
          },
        ]
      : [];
  return { data, state: { estimatedDelivery }, events };
}

function failure(code, message) {
  return Object.assign(new Error(message), { code });
}
