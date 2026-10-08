import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { chmod, mkdir, open, readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const scopes = ["user.info.basic", "user.info.stats", "video.list"];
const lifetime = 10 * 60 * 1000;
const fail = (message) => Object.assign(new Error(message), { safe: true });
const nonempty = (value) =>
  typeof value === "string" && value.trim().length > 0;

async function writePrivate(file, value) {
  await mkdir(dirname(file), { recursive: true, mode: 0o700 });
  const handle = await open(file, "wx", 0o600);
  try {
    await handle.writeFile(JSON.stringify(value, null, 2) + "\n");
    await handle.sync();
  } finally {
    await handle.close();
  }
  await chmod(file, 0o600);
}

function redirectUri(value, platform = "web") {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw fail("Use the registered absolute redirect URI.");
  }
  const desktop = platform === "desktop";
  if (
    (desktop
      ? !["http:", "https:"].includes(url.protocol) ||
        !["localhost", "127.0.0.1"].includes(url.hostname) ||
        !url.port ||
        Number(url.port) === 0
      : url.protocol !== "https:") ||
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    value.length >= 512
  )
    throw fail(
      desktop
        ? "Use a registered static loopback redirect URI with a port and no query or fragment."
        : "Use the registered static HTTPS redirect URI without a query or fragment.",
    );
  return value;
}

function validateSession(request) {
  const platform = request?.platform ?? "web";
  if (
    !nonempty(request?.clientKey) ||
    !nonempty(request.state) ||
    !["web", "desktop"].includes(platform) ||
    !Number.isSafeInteger(request.createdAt) ||
    request.createdAt > Date.now() ||
    Date.now() - request.createdAt >= lifetime ||
    (platform === "desktop" &&
      !/^[A-Za-z0-9._~-]{43,128}$/.test(request.codeVerifier ?? ""))
  )
    throw fail(
      "The authorization session is invalid or expired. Start a new authorization.",
    );
  redirectUri(request.redirectUri, platform);
  return request;
}

function callbackCode(request, text) {
  let callback;
  try {
    callback = new URL(text.trim());
  } catch {
    throw fail("The callback file must contain the complete redirect URL.");
  }
  const registered = new URL(request.redirectUri);
  if (
    callback.origin !== registered.origin ||
    callback.pathname !== registered.pathname ||
    callback.username ||
    callback.password ||
    callback.hash ||
    callback.searchParams.getAll("state").length !== 1
  )
    throw fail("The callback does not match this authorization session.");
  const actual = Buffer.from(callback.searchParams.get("state") ?? "");
  const expected = Buffer.from(request.state);
  if (actual.length !== expected.length || !timingSafeEqual(actual, expected))
    throw fail("The callback does not match this authorization session.");
  if (callback.searchParams.has("error"))
    throw Object.assign(
      fail("TikTok did not grant access. Start a new authorization."),
      { cancelled: true },
    );
  if (callback.searchParams.getAll("code").length !== 1)
    throw fail("The callback does not match this authorization session.");
  const code = callback.searchParams.get("code");
  if (!nonempty(code))
    throw fail("The callback contains no authorization code.");
  return code;
}

async function listen(request, callbackFile, onListening, signal) {
  const redirect = new URL(request.redirectUri);
  if (request.platform !== "desktop" || redirect.protocol !== "http:")
    throw fail("The listener requires a Desktop HTTP loopback session.");
  if (signal?.aborted) throw fail("The callback listener was cancelled.");
  await mkdir(dirname(callbackFile), { recursive: true, mode: 0o700 });
  const handle = await open(callbackFile, "wx", 0o600);
  return new Promise((resolve, reject) => {
    let settled = false;
    let accepting = true;
    let timer;
    const onAbort = () => finish(fail("The callback listener was cancelled."));
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      server.close();
      server.closeAllConnections();
      handle
        .close()
        .then(() => (error ? reject(error) : resolve(value)), reject);
    };
    const server = createServer(
      { maxHeaderSize: 8192, requestTimeout: 10000, headersTimeout: 10000 },
      (incoming, response) => {
        response.setHeader("Content-Type", "text/plain; charset=utf-8");
        response.setHeader("Cache-Control", "no-store");
        response.setHeader("Content-Security-Policy", "default-src 'none'");
        response.setHeader("X-Content-Type-Options", "nosniff");
        const refuse = () => {
          response.statusCode = 400;
          response.end(
            "This callback does not match the authorization session.",
          );
        };
        if (
          !accepting ||
          incoming.method !== "GET" ||
          incoming.headers.host !== redirect.host ||
          !incoming.url?.startsWith("/") ||
          incoming.url.startsWith("//") ||
          incoming.url.length > 8192
        ) {
          refuse();
          return;
        }
        let callback;
        try {
          callback = new URL(incoming.url, redirect);
          validateSession(request);
          callbackCode(request, callback.href);
        } catch (error) {
          if (error.cancelled) {
            accepting = false;
            response.end("TikTok access was not granted.", () => finish(error));
          } else refuse();
          return;
        }
        accepting = false;
        void (async () => {
          try {
            await handle.writeFile(callback.href + "\n");
            await handle.sync();
            response.end(
              "Authorization callback saved privately. You can close this tab.",
              () => finish(null, { callbackFile, captured: true }),
            );
          } catch {
            response.statusCode = 500;
            response.end("The private callback could not be saved.");
            finish(fail("The private callback could not be saved."));
          }
        })();
      },
    );
    signal?.addEventListener("abort", onAbort, { once: true });
    server.once("error", () =>
      finish(
        fail(
          "The loopback listener could not start. Check the registered port.",
        ),
      ),
    );
    timer = setTimeout(
      () =>
        finish(
          fail("The callback session expired. Start a new authorization."),
        ),
      request.createdAt + lifetime - Date.now(),
    );
    server.listen(Number(redirect.port), "127.0.0.1", () => {
      try {
        onListening({ listening: true, redirectUri: request.redirectUri });
      } catch {
        finish(fail("The loopback listener could not report readiness."));
      }
    });
  });
}

export async function authorize(args, { onListening = () => {}, signal } = {}) {
  const [action, ...arguments_] = args;
  const allowed = new Set([
    "--session-file",
    ...(action !== "listen" ? ["--client-file"] : []),
    ...(action === "begin" ? ["--redirect-uri", "--platform"] : []),
    ...(action === "finish" ? ["--callback-file", "--output"] : []),
    ...(action === "listen" ? ["--callback-file"] : []),
  ]);
  if (!["begin", "listen", "finish"].includes(action))
    throw fail(
      "Use begin, listen or finish; see the social collector setup guide.",
    );
  const options = {};
  for (let index = 0; index < arguments_.length; index += 2) {
    const key = arguments_[index];
    if (
      !allowed.has(key) ||
      !arguments_[index + 1] ||
      Object.hasOwn(options, key)
    )
      throw fail(
        "Use named file options; see the social collector setup guide.",
      );
    options[key] = arguments_[index + 1];
  }
  const required = (key) => {
    if (!options[key]) throw fail(`Missing ${key}.`);
    return options[key];
  };
  const sessionFile = resolve(required("--session-file"));
  if (action === "listen")
    return listen(
      validateSession(JSON.parse(await readFile(sessionFile, "utf8"))),
      resolve(required("--callback-file")),
      onListening,
      signal,
    );
  const credentials = JSON.parse(
    await readFile(resolve(required("--client-file")), "utf8"),
  );
  if (!nonempty(credentials?.clientKey) || !nonempty(credentials?.clientSecret))
    throw fail(
      "The private client file must contain clientKey and clientSecret.",
    );
  if (action === "begin") {
    const platform = options["--platform"] ?? "web";
    if (!["web", "desktop"].includes(platform))
      throw fail("Use web or desktop as the authorization platform.");
    const request = {
      clientKey: credentials.clientKey,
      platform,
      redirectUri: redirectUri(required("--redirect-uri"), platform),
      state: randomBytes(32).toString("base64url"),
      createdAt: Date.now(),
      ...(platform === "desktop"
        ? { codeVerifier: randomBytes(32).toString("base64url") }
        : {}),
    };
    const url = new URL("https://www.tiktok.com/v2/auth/authorize/");
    url.search = new URLSearchParams({
      client_key: request.clientKey,
      response_type: "code",
      scope: scopes.join(","),
      redirect_uri: request.redirectUri,
      state: request.state,
      disable_auto_auth: "1",
      ...(platform === "desktop"
        ? {
            code_challenge: createHash("sha256")
              .update(request.codeVerifier)
              .digest("hex"),
            code_challenge_method: "S256",
          }
        : {}),
    }).toString();
    await writePrivate(sessionFile, request);
    return { authorizationUrl: url.href };
  }
  const request = validateSession(
    JSON.parse(await readFile(sessionFile, "utf8")),
  );
  if (request.clientKey !== credentials.clientKey)
    throw fail(
      "The authorization session is invalid or expired. Start a new authorization.",
    );
  const code = callbackCode(
    request,
    await readFile(resolve(required("--callback-file")), "utf8"),
  );
  // Reserve storage and durably record intent before consuming a one-time code.
  const output = resolve(required("--output"));
  await writePrivate(output, {});
  try {
    await writePrivate(sessionFile + ".exchange", {
      output,
      startedAt: Date.now(),
    });
  } catch (error) {
    if (error.code === "EEXIST")
      throw fail(
        "This session already started a token exchange. Inspect its output; do not retry the code.",
      );
    throw error;
  }
  let response, body;
  try {
    response = await fetch("https://open.tiktokapis.com/v2/oauth/token/", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        client_key: credentials.clientKey,
        client_secret: credentials.clientSecret,
        grant_type: "authorization_code",
        code,
        redirect_uri: request.redirectUri,
        ...(request.platform === "desktop"
          ? { code_verifier: request.codeVerifier }
          : {}),
      }),
      signal: AbortSignal.timeout(30000),
      redirect: "error",
    });
    body = await response.json();
  } catch {
    throw fail(
      "The token exchange outcome is unknown. Inspect the private output or start a new authorization; do not retry the code.",
    );
  }
  if (!response.ok || body?.error)
    throw fail(
      "TikTok rejected the token exchange. Check the app and registered redirect URI, then start a new authorization.",
    );
  const issuedAt = Date.now();
  if (
    !nonempty(body?.access_token) ||
    !nonempty(body.refresh_token) ||
    !nonempty(body.open_id) ||
    typeof body.scope !== "string" ||
    typeof body.token_type !== "string" ||
    body.token_type.toLowerCase() !== "bearer" ||
    !Number.isSafeInteger(body.expires_in) ||
    body.expires_in <= 0 ||
    !Number.isSafeInteger(body.refresh_expires_in) ||
    body.refresh_expires_in <= 0 ||
    !Number.isSafeInteger(issuedAt + body.expires_in * 1000) ||
    !Number.isSafeInteger(issuedAt + body.refresh_expires_in * 1000)
  )
    throw fail(
      "TikTok returned an invalid token response. Start a new authorization.",
    );
  const granted = [
    ...new Set(
      body.scope
        .split(",")
        .map((value) => value.trim())
        .filter(Boolean),
    ),
  ];
  const record = {
    clientKey: credentials.clientKey,
    clientSecret: credentials.clientSecret,
    accessToken: body.access_token,
    refreshToken: body.refresh_token,
    openId: body.open_id,
    scopes: granted,
    issuedAt: new Date(issuedAt).toISOString(),
    expiresAt: new Date(issuedAt + body.expires_in * 1000).toISOString(),
    refreshExpiresAt: new Date(
      issuedAt + body.refresh_expires_in * 1000,
    ).toISOString(),
  };
  const handle = await open(output, "r+");
  try {
    await handle.truncate(0);
    await handle.writeFile(JSON.stringify(record, null, 2) + "\n");
    await handle.sync();
  } finally {
    await handle.close();
  }
  const complete = scopes.every((scope) => granted.includes(scope));
  return {
    output,
    complete,
    message: complete
      ? "Tokens were saved privately. Confirm the account and provision its collector; tokens were not printed."
      : "Tokens were saved privately, but required scopes are missing. Complete app approval and authorize again before enabling the collector.",
  };
}

if (
  process.argv[1] &&
  pathToFileURL(resolve(process.argv[1])).href === import.meta.url
) {
  try {
    process.stdout.write(
      JSON.stringify(
        await authorize(process.argv.slice(2), {
          onListening: (value) =>
            process.stdout.write(JSON.stringify(value) + "\n"),
        }),
      ) + "\n",
    );
  } catch (error) {
    const message = error.safe
      ? error.message
      : error.code === "ENOENT"
        ? "A required private file was not found."
        : error.code === "EEXIST"
          ? "The destination already exists; choose a new private file."
          : error instanceof SyntaxError
            ? "A private configuration file contains invalid JSON."
            : "Private authorization files could not be read or saved. Inspect them before starting a new authorization.";
    process.stderr.write(message + "\n");
    process.exitCode = 1;
  }
}
