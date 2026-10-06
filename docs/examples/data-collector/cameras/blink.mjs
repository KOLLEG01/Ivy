import { setTimeout as sleep } from "node:timers/promises";
import { spawn } from "node:child_process";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import {
  chmod,
  mkdir,
  open,
  readFile,
  rename,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { dirname, join } from "node:path";

const maximumTotalImageBytes = 650_000;
const groups = { camera: "cameras", mini: "owls", doorbell: "doorbells" };
class ApiError extends Error {
  constructor(message, code = "provider_unavailable") {
    super(message);
    this.code = code;
  }
}

const oauthOrigin = "https://api.oauth.blink.com";
const oauthRedirect = "immedia-blink://applinks.blink.com/signin/callback";
const oauthUserAgent =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.1 Mobile/15E148 Safari/604.1";
const tokenUserAgent = "Blink/2511191620 CFNetwork/3860.200.71 Darwin/25.1.0";
const hash = (value) => createHash("sha256").update(value).digest("hex");
const isUuid = (value) =>
  typeof value === "string" &&
  /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value);
const validAccount = (accountId, region) =>
  /^\d{1,20}$/.test(String(accountId ?? "")) &&
  /^[a-z0-9-]{1,20}$/.test(region ?? "");

async function privateCache(path) {
  try {
    if ((await stat(path)).size > 262144) throw new Error();
    const value = JSON.parse(await readFile(path, "utf8"));
    if (value.version !== 1 || !/^[a-f0-9]{64}$/.test(value.fingerprint))
      throw new Error();
    return value;
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw new ApiError(
      "Blink private authentication cache is invalid or unreadable; remove it to authorize again.",
      "configuration_invalid",
    );
  }
}

async function savePrivateCache(path, value) {
  const temporary = `${path}.${randomUUID()}.tmp`;
  let handle;
  try {
    await mkdir(dirname(path), { recursive: true, mode: 0o700 });
    await chmod(dirname(path), 0o700);
    handle = await open(temporary, "wx", 0o600);
    await handle.writeFile(JSON.stringify(value));
    await handle.sync();
    await handle.close();
    handle = null;
    await rename(temporary, path);
  } catch {
    throw new ApiError(
      "Blink private authentication cache could not be saved. Collection stopped to protect token rotation.",
      "configuration_invalid",
    );
  } finally {
    await handle?.close().catch(() => undefined);
    await rm(temporary, { force: true }).catch(() => undefined);
  }
}

async function authentication(
  config,
  secrets,
  signal,
  request,
  timeoutMs,
  directory,
) {
  const selected = (field, fallback) => {
    const name = config[field] ?? fallback;
    const value = name ? secrets[name] : undefined;
    if (value === undefined || value === "") return undefined;
    if (typeof value !== "string")
      throw new ApiError(
        "Blink authentication secrets must be strings.",
        "configuration_invalid",
      );
    return value;
  };
  const supplied = {
    access: selected("accessTokenSecretName", "BLINK_ACCESS_TOKEN"),
    refresh: selected("refreshTokenSecretName", "BLINK_REFRESH_TOKEN"),
    email: selected("emailSecretName", "BLINK_EMAIL"),
    password: selected("passwordSecretName", "BLINK_PASSWORD"),
    hardwareId: selected("hardwareIdSecretName", "BLINK_HARDWARE_ID"),
  };
  const mfaCode = selected("mfaCodeSecretName", "BLINK_MFA_CODE")?.trim();
  if (Boolean(supplied.email) !== Boolean(supplied.password))
    throw new ApiError(
      "Blink email and password must both be selected for password fallback.",
      "configuration_invalid",
    );
  if (supplied.hardwareId && !isUuid(supplied.hardwareId))
    throw new ApiError(
      "Blink hardware id must be the UUID from the authorized session.",
      "configuration_invalid",
    );
  if (!supplied.access && !supplied.refresh && !supplied.email)
    throw new ApiError(
      "Blink needs a selected access token, refresh token or email/password pair.",
      "authentication_required",
    );
  signal?.throwIfAborted();
  const path = join(directory, ".auth", "blink.json");
  // An OTP changes while its login session stays the same, so it is deliberately
  // excluded from the durable credential fingerprint and tracked by consumption hash.
  const fingerprint = hash(JSON.stringify(supplied));
  let cache = await privateCache(path);
  if (cache && cache.fingerprint !== fingerprint) {
    await rm(path).catch(() => {
      throw new ApiError(
        "Blink private authentication cache could not be reset.",
        "configuration_invalid",
      );
    });
    cache = null;
  }
  if (supplied.refresh && !supplied.hardwareId && !cache?.hardwareId)
    throw new ApiError(
      "A supplied Blink refresh token requires its matching hardware-id secret.",
      "configuration_invalid",
    );
  cache ??= {
    version: 1,
    fingerprint,
    hardwareId: supplied.hardwareId ?? randomUUID().toUpperCase(),
  };
  if (!isUuid(cache.hardwareId))
    throw new ApiError(
      "Blink private authentication cache has an invalid device identity; remove it to authorize again.",
      "configuration_invalid",
    );
  const managed = Boolean(supplied.refresh || supplied.email);
  const persist = async () => {
    if (managed) await savePrivateCache(path, cache);
  };
  let token = cache.accessToken ?? supplied.access;
  if (
    supplied.access &&
    cache.rejectedAccessHash === hash(supplied.access) &&
    !cache.accessToken
  )
    token = undefined;

  const send = async (
    url,
    options = {},
    jar,
    commitCompletedResponse = false,
  ) => {
    signal?.throwIfAborted();
    const timeout = AbortSignal.timeout(timeoutMs);
    const combined = signal ? AbortSignal.any([signal, timeout]) : timeout;
    try {
      const headers = { ...options.headers };
      if (jar) headers.Cookie = await jar.getCookieString(url);
      const response = await request(url, {
        ...options,
        headers,
        redirect: "manual",
        signal: combined,
      });
      if (jar)
        for (const cookie of response.headers.getSetCookie())
          await jar.setCookie(cookie, url, { ignoreError: true });
      const bytes = response.body
        ? await readBytes(response, 262144)
        : Buffer.alloc(0);
      if (combined.aborted && !commitCompletedResponse)
        combined.throwIfAborted();
      return { response, text: bytes.toString("utf8") };
    } catch (error) {
      signal?.throwIfAborted();
      if (combined.aborted)
        throw new ApiError("Blink authentication request timed out.");
      if (error instanceof ApiError) throw error;
      throw new ApiError("Blink authentication network request failed.");
    }
  };
  const decode = (text) => {
    try {
      return JSON.parse(text);
    } catch {
      throw new ApiError("Blink authentication returned invalid JSON.");
    }
  };
  const tokenRequest = async (fields, jar) => {
    const { response, text } = await send(
      `${oauthOrigin}/oauth/token`,
      {
        method: "POST",
        headers: {
          "User-Agent": tokenUserAgent,
          "Content-Type": "application/x-www-form-urlencoded",
          Accept: "*/*",
        },
        body: new URLSearchParams({
          client_id: "ios",
          scope: "client",
          hardware_id: cache.hardwareId,
          ...fields,
        }).toString(),
      },
      jar,
      true,
    );
    if ([400, 401, 403].includes(response.status))
      throw new ApiError(
        "Blink credentials were rejected or revoked; provide a new token or complete password authorization.",
        "authentication_required",
      );
    if (response.status === 412)
      throw new ApiError(
        "Blink requires account verification before granting a token.",
        "interaction_required",
      );
    if (!response.ok)
      throw new ApiError(
        response.status === 429
          ? "Blink authentication is rate limited; wait before retrying."
          : "Blink token service is unavailable.",
      );
    const value = decode(text);
    if (
      !value ||
      typeof value.access_token !== "string" ||
      !value.access_token ||
      typeof value.refresh_token !== "string" ||
      !value.refresh_token ||
      !Number.isFinite(value.expires_in) ||
      value.expires_in <= 0
    )
      throw new ApiError("Blink token response is incomplete.");
    cache.accessToken = value.access_token;
    cache.refreshToken = value.refresh_token;
    cache.expiresAt = Date.now() + value.expires_in * 1000;
    delete cache.pending;
    delete cache.revokedRefreshHash;
    // Commit rotation before any tier lookup or camera/motion request, even if
    // cancellation arrived after the complete token response was received.
    await persist();
    token = value.access_token;
    signal?.throwIfAborted();
    return token;
  };
  const passwordLogin = async () => {
    if (!supplied.email)
      throw new ApiError(
        "Blink needs renewed credentials.",
        "authentication_required",
      );
    if (cache.rejectedPassword)
      throw new ApiError(
        "Blink rejected these password credentials; change them before retrying.",
        "authentication_required",
      );
    if (cache.signinAttempted && !cache.pending)
      throw new ApiError(
        "Blink signin was interrupted or needs an unsupported security challenge. Remove the private cache after owner verification to authorize again.",
        "interaction_required",
      );
    let CookieJar, parse;
    try {
      [{ CookieJar }, { parse }] = await Promise.all([
        import("tough-cookie"),
        import("parse5"),
      ]);
    } catch {
      throw new ApiError(
        "Install the declared Blink cookie and HTML parser dependencies for password authorization.",
        "configuration_invalid",
      );
    }
    let pending = cache.pending;
    if (
      pending &&
      (!Number.isFinite(pending.createdAt) ||
        Date.now() - pending.createdAt > 15 * 60_000)
    )
      throw new ApiError(
        "Blink MFA session expired. Remove the private cache to start a new authorization.",
        "interaction_required",
      );
    let jar;
    try {
      jar = pending
        ? await CookieJar.deserialize(pending.cookies)
        : new CookieJar();
    } catch {
      throw new ApiError(
        "Blink private MFA cookie session is invalid; remove the cache to authorize again.",
        "configuration_invalid",
      );
    }
    const page = async (url) => {
      for (let redirects = 0; redirects < 8; redirects++) {
        const result = await send(
          url,
          {
            headers: {
              "User-Agent": oauthUserAgent,
              Accept:
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
              "Accept-Language": "en-US,en;q=0.9",
            },
          },
          jar,
        );
        if (![301, 302, 303, 307, 308].includes(result.response.status))
          return result;
        let next;
        try {
          const location = result.response.headers.get("Location");
          if (!location) throw new Error();
          next = new URL(location, url);
        } catch {
          throw new ApiError(
            "Blink returned an invalid authorization redirect.",
            "interaction_required",
          );
        }
        if (next.origin !== oauthOrigin)
          throw new ApiError(
            "Blink requires an unsupported redirect or security challenge. Supply an authorized refresh token.",
            "interaction_required",
          );
        url = next.href;
      }
      throw new ApiError(
        "Blink authorization redirect limit reached.",
        "interaction_required",
      );
    };
    const form = async (path, fields) =>
      send(
        `${oauthOrigin}${path}`,
        {
          method: "POST",
          headers: {
            "User-Agent": oauthUserAgent,
            Accept: "*/*",
            "Content-Type": "application/x-www-form-urlencoded",
            Origin: oauthOrigin,
            Referer: `${oauthOrigin}/oauth/v2/signin`,
          },
          body: new URLSearchParams(fields).toString(),
        },
        jar,
      );
    if (!pending) {
      await persist(); // Stable device identity survives an interrupted login.
      const verifier = randomBytes(32).toString("base64url");
      const challenge = createHash("sha256")
        .update(verifier)
        .digest("base64url");
      const params = new URLSearchParams({
        app_brand: "blink",
        app_version: "50.1",
        client_id: "ios",
        code_challenge: challenge,
        code_challenge_method: "S256",
        device_brand: "Apple",
        device_model: "iPhone16,1",
        device_os_version: "26.1",
        hardware_id: cache.hardwareId,
        redirect_uri: oauthRedirect,
        response_type: "code",
        scope: "client",
      });
      const authorized = await page(
        `${oauthOrigin}/oauth/v2/authorize?${params}`,
      );
      if (authorized.response.status !== 200)
        throw new ApiError(
          "Blink password authorization requires a security challenge or is unavailable.",
          authorized.response.status === 403
            ? "interaction_required"
            : "provider_unavailable",
        );
      const signin = await page(`${oauthOrigin}/oauth/v2/signin`);
      if (signin.response.status !== 200)
        throw new ApiError("Blink signin page is unavailable.");
      const stack = [parse(signin.text)];
      let csrfToken;
      while (stack.length) {
        const node = stack.pop();
        if (
          node.tagName === "script" &&
          node.attrs?.some(
            (attribute) =>
              attribute.name === "id" && attribute.value === "oauth-args",
          )
        ) {
          const args = decode(
            node.childNodes.map((child) => child.value ?? "").join(""),
          );
          csrfToken = args?.["csrf-token"];
          break;
        }
        stack.push(...(node.childNodes ?? []));
      }
      if (typeof csrfToken !== "string" || !csrfToken)
        throw new ApiError(
          "Blink requires an unsupported signin or CAPTCHA/security challenge. Supply an authorized refresh token.",
          "interaction_required",
        );
      // A lost signin response might already have sent an OTP. Do not repeat it
      // automatically on every schedule when its resulting challenge is unknown.
      cache.signinAttempted = true;
      await persist();
      const signed = await form("/oauth/v2/signin", {
        username: supplied.email,
        password: supplied.password,
        "csrf-token": csrfToken,
      });
      const challengeData =
        signed.response.status === 202 ? decode(signed.text) : {};
      const needsCode =
        signed.response.status === 412 ||
        (signed.response.status === 202 &&
          Boolean(
            challengeData?.tsv_state ||
            challengeData?.tsv_methods ||
            challengeData?.next_time_in_secs,
          ));
      if (
        !needsCode &&
        ![301, 302, 303, 307, 308].includes(signed.response.status)
      ) {
        if ([400, 401].includes(signed.response.status)) {
          cache.rejectedPassword = true;
          await persist();
        }
        throw new ApiError(
          "Blink password signin failed or requires an unsupported challenge.",
          [400, 401].includes(signed.response.status)
            ? "authentication_required"
            : "interaction_required",
        );
      }
      pending = {
        verifier,
        csrfToken,
        createdAt: Date.now(),
        verified: !needsCode,
        cookies: await jar.serialize(),
      };
      cache.pending = pending;
      delete cache.signinAttempted;
      await persist();
    }
    if (!pending.verified) {
      if (!mfaCode)
        throw new ApiError(
          "Blink MFA requires the received verification code in the selected MFA secret.",
          "interaction_required",
        );
      if (!/^\d{6,8}$/.test(mfaCode))
        throw new ApiError(
          "Blink MFA code must contain six to eight digits.",
          "configuration_invalid",
        );
      const codeHash = hash(mfaCode);
      if (cache.consumedMfaCodeHash === codeHash)
        throw new ApiError(
          "Blink MFA code was already attempted; supply a new received code.",
          "interaction_required",
        );
      cache.consumedMfaCodeHash = codeHash;
      await persist(); // An uncertain verification must not replay this code on a timer.
      const verified = await form("/oauth/v2/2fa/verify", {
        "2fa_code": mfaCode,
        "csrf-token": pending.csrfToken,
        remember_me: "false",
      });
      pending.cookies = await jar.serialize();
      if (
        verified.response.status !== 201 ||
        decode(verified.text)?.status !== "auth-completed"
      ) {
        await persist();
        throw new ApiError(
          "Blink MFA verification failed; supply a new received code.",
          "interaction_required",
        );
      }
      pending.verified = true;
      await persist();
    }
    const authorization = await send(
      `${oauthOrigin}/oauth/v2/authorize`,
      {
        headers: {
          "User-Agent": oauthUserAgent,
          Accept: "*/*",
          Referer: `${oauthOrigin}/oauth/v2/signin`,
        },
      },
      jar,
    );
    let code;
    if ([301, 302, 303, 307, 308].includes(authorization.response.status)) {
      try {
        const redirect = new URL(
          authorization.response.headers.get("Location"),
          oauthOrigin,
        );
        if (
          `${redirect.protocol}//${redirect.host}${redirect.pathname}` ===
          oauthRedirect
        )
          code = redirect.searchParams.get("code");
      } catch {
        /* Invalid redirect, handled below. */
      }
    }
    if (!code)
      throw new ApiError(
        "Blink authorization did not return a code; the session needs user verification.",
        "interaction_required",
      );
    return tokenRequest(
      {
        grant_type: "authorization_code",
        app_brand: "blink",
        code,
        code_verifier: pending.verifier,
        redirect_uri: oauthRedirect,
      },
      jar,
    );
  };
  const renew = async () => {
    delete cache.accessToken;
    cache.expiresAt = 0;
    if (supplied.access) cache.rejectedAccessHash = hash(supplied.access);
    const refresh = cache.refreshToken ?? supplied.refresh;
    if (refresh && cache.revokedRefreshHash !== hash(refresh)) {
      try {
        return await tokenRequest({
          grant_type: "refresh_token",
          refresh_token: refresh,
        });
      } catch (error) {
        if (
          !(error instanceof ApiError) ||
          error.code !== "authentication_required"
        )
          throw error;
        cache.revokedRefreshHash = hash(refresh);
        delete cache.refreshToken;
        await persist();
        if (!supplied.email) throw error;
      }
    }
    return passwordLogin();
  };
  if (
    !token ||
    (Number.isFinite(cache.expiresAt) && cache.expiresAt - Date.now() < 60_000)
  )
    await renew();
  return {
    get token() {
      return token;
    },
    canRenew: managed,
    renew,
    async account() {
      if (validAccount(config.accountId, config.region))
        return { accountId: String(config.accountId), region: config.region };
      if (validAccount(cache.accountId, cache.region))
        return { accountId: cache.accountId, region: cache.region };
      const tier = () =>
        send("https://rest-prod.immedia-semi.com/api/v1/users/tier_info", {
          headers: {
            Authorization: `Bearer ${token}`,
            "User-Agent": "27.0ANDROID_28373244",
            "Content-Type": "application/x-www-form-urlencoded",
          },
        });
      let result = await tier();
      if (result.response.status === 401 && managed) {
        await renew();
        result = await tier();
      }
      const { response, text } = result;
      if (!response.ok)
        throw new ApiError(
          "Blink account discovery failed; configure accountId and region from the authorized session.",
          [401, 403].includes(response.status)
            ? "authentication_required"
            : "provider_unavailable",
        );
      const value = decode(text);
      if (!validAccount(value?.account_id, value?.tier))
        throw new ApiError(
          "Blink returned an unsupported account tier response.",
        );
      cache.accountId = String(value.account_id);
      cache.region = value.tier;
      await persist();
      return { accountId: cache.accountId, region: cache.region };
    },
  };
}

function integer(value, fallback, minimum, maximum, name) {
  const result = value ?? fallback;
  if (!Number.isInteger(result) || result < minimum || result > maximum)
    throw new ApiError(
      `${name} must be an integer from ${minimum} to ${maximum}.`,
      "configuration_invalid",
    );
  return result;
}

async function readBytes(response, limit) {
  const chunks = [];
  let size = 0;
  const reader = response.body?.getReader();
  if (!reader) throw new ApiError("Blink returned an empty response.");
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) {
        await reader.cancel();
        throw new ApiError("Blink response exceeds its size limit.");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  return Buffer.concat(chunks);
}

const completeJpeg = (bytes) =>
  bytes.length >= 4 &&
  bytes[0] === 0xff &&
  bytes[1] === 0xd8 &&
  bytes.at(-2) === 0xff &&
  bytes.at(-1) === 0xd9;
const cameraKey = (camera) => `${camera.networkId}:${camera.type}:${camera.id}`;
const localClipKey = (clip) =>
  `${clip.syncModuleId}:${clip.clipId}:${clip.recordedAt}`;

// Decode a saved file only. image2's update mode leaves the last decoded frame
// on disk without buffering the entire decoded video in memory.
export async function extractLastClipFrame(
  options,
  signal,
  spawnProcess = spawn,
) {
  signal?.throwIfAborted();
  const offsetSeconds = await new Promise((resolve, reject) => {
    const args = [
      "-hide_banner",
      "-loglevel",
      "quiet",
      "-nostdin",
      "-y",
      "-protocol_whitelist",
      "file,pipe",
      "-threads",
      "1",
      "-f",
      "mov",
      "-i",
      options.inputPath,
      "-map",
      "0:v:0",
      "-an",
      "-vf",
      `scale=w='min(${options.width},iw)':h=-2`,
      "-threads",
      "1",
      "-c:v",
      "mjpeg",
      "-q:v",
      String(options.quality),
      "-fps_mode",
      "passthrough",
      "-update",
      "1",
      "-f",
      "image2",
      options.outputPath,
      "-progress",
      "pipe:1",
      "-nostats",
    ];
    let child;
    try {
      child = spawnProcess(options.ffmpegPath, args, {
        shell: false,
        windowsHide: true,
        stdio: ["ignore", "pipe", "ignore"],
      });
    } catch {
      reject(new ApiError("Cannot start FFmpeg for saved Blink clips."));
      return;
    }
    let failure,
      offset,
      pending = "";
    const stop = (message) => {
      if (failure) return;
      failure = new ApiError(message);
      child.kill("SIGKILL");
    };
    const onAbort = () => stop("Blink clip extraction was cancelled.");
    const timer = setTimeout(
      () => stop("Blink clip extraction timed out."),
      options.timeoutMs,
    );
    const cleanup = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
    };
    signal?.addEventListener("abort", onAbort, { once: true });
    if (signal?.aborted) onAbort();
    child.stdout.on("data", (chunk) => {
      if (failure) return;
      pending += chunk.toString("utf8");
      if (pending.length > 16384) {
        stop("FFmpeg returned oversized clip progress.");
        return;
      }
      let end;
      while ((end = pending.indexOf("\n")) >= 0) {
        const line = pending.slice(0, end).trim();
        pending = pending.slice(end + 1);
        const match = /^out_time_us=(\d+)$/.exec(line);
        if (match) {
          if (Number(match[1]) > 600_000_000)
            stop("Blink saved clip exceeds the extraction duration limit.");
          else offset = Number(match[1]) / 1_000_000;
        }
      }
    });
    child.once("error", () => {
      cleanup();
      reject(new ApiError("Cannot start FFmpeg for saved Blink clips."));
    });
    child.once("close", (code) => {
      cleanup();
      if (failure) reject(failure);
      else if (code !== 0 || !Number.isFinite(offset))
        reject(new ApiError("FFmpeg could not extract the saved Blink clip."));
      else resolve(offset);
    });
  });
  if ((await stat(options.outputPath)).size > options.maximumImageBytes)
    throw new ApiError("Blink clip frame exceeds maximumImageBytes.");
  const bytes = await readFile(options.outputPath);
  if (!completeJpeg(bytes))
    throw new ApiError("FFmpeg did not produce a complete Blink clip JPEG.");
  return { bytes, offsetSeconds };
}

async function cachedClipFrame(path, maximumImageBytes) {
  try {
    if ((await stat(path)).size > Math.ceil((maximumImageBytes * 4) / 3) + 4096)
      return null;
    const value = JSON.parse(await readFile(path, "utf8"));
    if (
      value.version !== 1 ||
      typeof value.clipKey !== "string" ||
      !Number.isFinite(Date.parse(value.recordedAt)) ||
      !Number.isFinite(Date.parse(value.capturedAt)) ||
      typeof value.jpeg !== "string"
    )
      return null;
    const bytes = Buffer.from(value.jpeg, "base64");
    if (bytes.length > maximumImageBytes || !completeJpeg(bytes)) return null;
    return { ...value, bytes, source: "blink-local-clip" };
  } catch {
    return null;
  }
}

async function storageCommand(api, networkId, path, timeoutMs, signal) {
  const deadline = Date.now() + timeoutMs;
  const command = await api(path, { method: "POST", deadline });
  if (!/^\d+$/.test(String(command?.id ?? "")))
    throw new ApiError("Blink did not accept the local storage command.");
  let complete = false;
  do {
    const status = await api(`/network/${networkId}/command/${command.id}`, {
      deadline,
    });
    if (status.status_code !== 908)
      throw new ApiError("Blink local storage is busy or the command failed.");
    complete = status.complete === true;
    if (!complete)
      await sleep(
        Math.max(1, Math.min(1000, deadline - Date.now())),
        undefined,
        { signal },
      );
  } while (!complete && Date.now() < deadline);
  if (!complete) throw new ApiError("Blink local storage command timed out.");
  return command.id;
}

async function downloadClipFrame({
  api,
  clip,
  config,
  options,
  path,
  signal,
  extractFrame,
}) {
  const directory = dirname(path);
  await mkdir(directory, { recursive: true, mode: 0o700 });
  await chmod(directory, 0o700);
  const temporary = join(directory, randomUUID());
  const inputPath = `${temporary}.mp4`,
    outputPath = `${temporary}.jpg`;
  const cacheTemporary = `${temporary}.json`;
  try {
    const route = `/api/v1/accounts/${config.accountId}/networks/${clip.networkId}/sync_modules/${clip.syncModuleId}/local_storage/manifest/${clip.manifestId}/clip/request/${clip.clipId}`;
    await storageCommand(
      api,
      clip.networkId,
      route,
      options.storageTimeoutMs,
      signal,
    );
    const video = await api(route, { video: true });
    await writeFile(inputPath, video, { flag: "wx", mode: 0o600 });
    await writeFile(outputPath, "", { flag: "wx", mode: 0o600 });
    const frame = await extractFrame(
      { ...options, inputPath, outputPath },
      signal,
    );
    const capturedAt = new Date(
      Date.parse(clip.recordedAt) + frame.offsetSeconds * 1000,
    ).toISOString();
    const value = {
      version: 1,
      clipKey: localClipKey(clip),
      clipId: clip.clipId,
      recordedAt: clip.recordedAt,
      capturedAt,
      jpeg: frame.bytes.toString("base64"),
    };
    await writeFile(cacheTemporary, JSON.stringify(value), {
      flag: "wx",
      mode: 0o600,
    });
    await rename(cacheTemporary, path);
    return { ...value, bytes: frame.bytes, source: "blink-local-clip" };
  } finally {
    for (const file of [inputPath, outputPath, cacheTemporary])
      await rm(file, { force: true }).catch(() => undefined);
  }
}

function cameraEntry(homescreen, camera) {
  const entries = homescreen[groups[camera.type]];
  const entry =
    Array.isArray(entries) &&
    entries.find(
      (item) =>
        String(item.id) === camera.id &&
        String(item.network_id) === camera.networkId,
    );
  if (!entry)
    throw new Error(
      "The configured camera was not found in the Blink homescreen.",
    );
  return entry;
}

function thumbnailPath(entry, camera, accountId) {
  const thumbnail = String(entry.thumbnail ?? "");
  if (/^\d+$/.test(thumbnail)) {
    // The homescreen exposes the media device type for numeric thumbnail timestamps.
    const mediaType = String(
      entry.type ??
        { camera: "camera", mini: "owl", doorbell: "doorbell" }[camera.type],
    );
    if (!/^[a-z0-9_-]+$/i.test(mediaType))
      throw new Error("Blink returned an invalid media device type.");
    return `/api/v3/media/accounts/${accountId}/networks/${camera.networkId}/${mediaType}/${camera.id}/thumbnail/thumbnail.jpg?ts=${thumbnail}&ext=`;
  }
  if (!thumbnail) throw new Error("Blink has no thumbnail for this camera.");
  if (thumbnail.endsWith("&ext=") || /\.jpe?g(?:\?|$)/i.test(thumbnail))
    return thumbnail;
  return `${thumbnail}.jpg`;
}

async function motionPoll({ api, cameras, config, state, startedAt }) {
  const previous = state?.motion;
  const overlapSeconds = integer(
    config.motionOverlapSeconds,
    300,
    30,
    3600,
    "motionOverlapSeconds",
  );
  const maximumLookbackSeconds = integer(
    config.maximumMotionLookbackSeconds,
    86_400,
    60,
    604_800,
    "maximumMotionLookbackSeconds",
  );
  const maxPages = integer(
    config.maximumMotionPages,
    10,
    1,
    20,
    "maximumMotionPages",
  );
  const previousTime = Date.parse(previous?.checkedAt);
  const sinceMs = Math.max(
    Number.isFinite(previousTime)
      ? previousTime - overlapSeconds * 1000
      : startedAt - overlapSeconds * 1000,
    startedAt - maximumLookbackSeconds * 1000,
  );
  const since = new Date(sinceMs).toISOString().replace(/\.\d{3}Z$/, "+0000");
  const previousIds = Array.isArray(previous?.seenIds)
    ? previous.seenIds
        .filter(
          (id) =>
            typeof id === "string" &&
            /^\d+:(camera|mini|doorbell):\d+:\d+$/.test(id),
        )
        .slice(0, 1000)
    : [];
  const seen = new Set(previousIds);
  const newIds = [],
    events = [];
  for (let page = 1; page <= maxPages; page++) {
    const response = await api(
      `/api/v1/accounts/${config.accountId}/media/changed?${new URLSearchParams({ since, page: String(page) })}`,
    );
    if (!Array.isArray(response.media))
      throw new Error(
        "Blink motion media is unavailable or has an unsupported response shape.",
      );
    for (const item of response.media) {
      const camera = cameras.find(
        (candidate) =>
          candidate.id === String(item.device_id) &&
          candidate.networkId === String(item.network_id),
      );
      if (
        !camera ||
        item.deleted === true ||
        item.type !== "video" ||
        item.source !== "pir"
      )
        continue;
      const mediaId = String(item.id ?? "");
      if (
        !/^\d{1,20}$/.test(mediaId) ||
        !Number.isFinite(Date.parse(item.created_at))
      )
        continue;
      const id = `${camera.networkId}:${camera.type}:${camera.id}:${mediaId}`;
      if (!seen.has(id)) {
        seen.add(id);
        newIds.push(id);
        // The first successful poll establishes a baseline without historical notices.
        if (Number.isFinite(previousTime))
          events.push({
            name: "motion",
            payload: {
              cameraId: camera.id,
              name: camera.name,
              mediaId,
              detectedAt: new Date(Date.parse(item.created_at)).toISOString(),
              source: "blink-cloud",
            },
          });
        if (newIds.length > 1000 || events.length > 64)
          throw new Error(
            "Blink motion batch exceeds its result budget; the cursor was preserved. Shorten the interval or recovery lookback.",
          );
      }
    }
    const pageSize =
      Number.isInteger(response.limit) && response.limit > 0
        ? response.limit
        : 25;
    if (response.media.length < pageSize) {
      return {
        events,
        state: {
          checkedAt: new Date(startedAt).toISOString(),
          seenIds: [...newIds, ...previousIds].slice(0, 1000),
        },
      };
    }
  }
  throw new Error(
    "Blink motion pagination exceeded maximumMotionPages; the cursor was preserved.",
  );
}

async function localStoragePoll({
  api,
  cameras,
  homescreen,
  config,
  state,
  startedAt,
  signal,
  timeoutMs,
}) {
  const modules = (homescreen.sync_modules ?? []).filter(
    (module) =>
      module.local_storage_enabled === true &&
      module.local_storage_status === "active" &&
      /^\d{1,20}$/.test(String(module.id)) &&
      cameras.some((camera) => camera.networkId === String(module.network_id)),
  );
  if (!modules.length)
    throw new Error("No active local storage for the configured cameras.");
  const next = {},
    events = [],
    latestClips = {};
  let clipCount = 0;
  const maximumLookback = integer(
    config.maximumMotionLookbackSeconds,
    86400,
    60,
    604800,
    "maximumMotionLookbackSeconds",
  );
  const normalizedName = (name) =>
    String(name ?? "").replace(/[^\p{L}\p{N}_]+/gu, "");
  for (const module of modules) {
    const networkId = String(module.network_id);
    const key = `${networkId}:${module.id}`;
    const previous = state?.localStorage?.[key];
    const previousIds = Array.isArray(previous?.seenIds)
      ? previous.seenIds
      : [];
    const seen = new Set(previousIds);
    const path = `/api/v1/accounts/${config.accountId}/networks/${networkId}/sync_modules/${module.id}/local_storage/manifest/request`;
    const commandId = await storageCommand(
      api,
      networkId,
      path,
      timeoutMs,
      signal,
    );
    const manifest = await api(`${path}/${commandId}`);
    if (
      !Array.isArray(manifest.clips) ||
      !/^\d{1,20}$/.test(String(manifest.manifest_id ?? ""))
    )
      throw new Error("Blink returned an invalid local storage manifest.");
    clipCount += manifest.clips.length;
    const clips = manifest.clips
      .map((clip) => ({
        ...clip,
        time: Date.parse(clip.created_at),
      }))
      .sort((a, b) => b.time - a.time);
    const currentIds = [];
    for (const clip of clips) {
      if (!/^\d{1,20}$/.test(String(clip.id)) || !Number.isFinite(clip.time))
        throw new Error(
          "Blink returned invalid local clip metadata; the cursor was preserved.",
        );
      const clipKey = `${clip.id}:${clip.time}`;
      currentIds.push(clipKey);
      // The manifest supplies a provider-normalized name, but no camera id or
      // motion source. Match the current provider name, never a display override.
      const matches = cameras.filter(
        (camera) =>
          camera.networkId === networkId &&
          normalizedName(cameraEntry(homescreen, camera).name) ===
            clip.camera_name,
      );
      if (matches.length > 1)
        throw new Error(
          "Local clip camera names are ambiguous; the cursor was preserved.",
        );
      if (matches.length === 1 && clip.time <= startedAt) {
        const key = cameraKey(matches[0]);
        if (
          !latestClips[key] ||
          clip.time > Date.parse(latestClips[key].recordedAt)
        )
          latestClips[key] = {
            networkId,
            syncModuleId: String(module.id),
            manifestId: String(manifest.manifest_id),
            clipId: String(clip.id),
            recordedAt: new Date(clip.time).toISOString(),
          };
      }
      if (seen.has(clipKey)) continue;
      seen.add(clipKey);
      if (
        !Number.isFinite(Date.parse(previous?.checkedAt)) ||
        clip.time < startedAt - maximumLookback * 1000 ||
        clip.time > startedAt
      )
        continue;
      if (matches.length === 1)
        events.push({
          name: "recording",
          payload: {
            cameraId: matches[0].id,
            name: matches[0].name,
            clipId: String(clip.id),
            recordedAt: new Date(clip.time).toISOString(),
            source: "blink-local-storage",
          },
        });
      if (events.length > 64)
        throw new Error(
          "Blink local clip batch exceeds the event budget; the cursor was preserved.",
        );
    }
    next[key] = {
      checkedAt: new Date(startedAt).toISOString(),
      seenIds: [...new Set([...currentIds, ...previousIds])].slice(0, 2000),
    };
  }
  return { state: next, events, clipCount, latestClips };
}

export default async function collect(
  { config, secrets, state, signal },
  request = fetch,
  cacheDirectory = process.cwd(),
  extractFrame = extractLastClipFrame,
) {
  if (
    (config.accountId !== undefined || config.region !== undefined) &&
    !validAccount(config.accountId, config.region)
  )
    throw new ApiError(
      "Configure both Blink accountId and region, or omit both for account discovery.",
      "configuration_invalid",
    );
  const configuredCameras = config.cameras ?? [];
  if (
    (!config.discoveryOnly &&
      (!Array.isArray(configuredCameras) ||
        configuredCameras.length < 1 ||
        configuredCameras.length > 5)) ||
    (config.discoveryOnly && !Array.isArray(configuredCameras))
  )
    throw new ApiError(
      "Configure one to five cameras; one task per camera is recommended.",
      "configuration_invalid",
    );
  const ids = new Set();
  const cameras = configuredCameras.map((camera) => {
    if (
      !camera ||
      !/^\d{1,20}$/.test(camera.id ?? "") ||
      !/^\d{1,20}$/.test(camera.networkId ?? "") ||
      !Object.hasOwn(groups, camera.type) ||
      ids.has(`${camera.networkId}:${camera.type}:${camera.id}`)
    )
      throw new ApiError(
        "Configure unique numeric camera ids, networkIds and camera, mini or doorbell types.",
        "configuration_invalid",
      );
    ids.add(`${camera.networkId}:${camera.type}:${camera.id}`);
    return {
      ...camera,
      id: String(camera.id),
      networkId: String(camera.networkId),
    };
  });
  const requestTimeoutMs = integer(
    config.requestTimeoutMs,
    10_000,
    100,
    30_000,
    "requestTimeoutMs",
  );
  const snapshotTimeoutMs = integer(
    config.snapshotTimeoutMs,
    20_000,
    100,
    60_000,
    "snapshotTimeoutMs",
  );
  const maximumImageBytes = integer(
    config.maximumImageBytes,
    524_288,
    1024,
    614_400,
    "maximumImageBytes",
  );
  const snapshotIntervalSeconds = integer(
    config.snapshotIntervalSeconds,
    0,
    0,
    86400,
    "snapshotIntervalSeconds",
  );
  const useLocalClipImages = config.localClipImages === true;
  if (
    useLocalClipImages &&
    (config.localStorage !== true || config.freshSnapshots !== false)
  )
    throw new ApiError(
      "localClipImages requires localStorage: true and freshSnapshots: false.",
      "configuration_invalid",
    );
  const maximumClipBytes = useLocalClipImages
    ? integer(
        config.maximumClipBytes,
        16_777_216,
        1024,
        33_554_432,
        "maximumClipBytes",
      )
    : 0;
  const clipOptions = useLocalClipImages
    ? {
        ffmpegPath: config.ffmpegPath ?? "ffmpeg",
        width: integer(config.clipFrameWidth, 960, 160, 1920, "clipFrameWidth"),
        quality: integer(config.clipFrameQuality, 7, 2, 31, "clipFrameQuality"),
        timeoutMs: integer(
          config.clipFrameTimeoutMs,
          10000,
          100,
          60000,
          "clipFrameTimeoutMs",
        ),
        storageTimeoutMs: snapshotTimeoutMs,
        maximumImageBytes,
      }
    : null;
  if (
    useLocalClipImages &&
    (typeof clipOptions.ffmpegPath !== "string" || !clipOptions.ffmpegPath)
  )
    throw new ApiError(
      "Configure an FFmpeg executable for local clip images.",
      "configuration_invalid",
    );
  const auth = await authentication(
    config,
    secrets,
    signal,
    request,
    requestTimeoutMs,
    cacheDirectory,
  );
  config = { ...config, ...(await auth.account()) };
  const base = new URL(`https://rest-${config.region}.immedia-semi.com`);
  const api = async (
    path,
    { method = "GET", image = false, video = false, deadline } = {},
  ) => {
    const url = new URL(path, base);
    if (url.origin !== base.origin || url.username || url.password)
      throw new ApiError(
        "Blink returned a thumbnail outside the configured API origin.",
      );
    const duration = Math.max(
      1,
      Math.min(
        requestTimeoutMs,
        deadline ? deadline - Date.now() : requestTimeoutMs,
      ),
    );
    let timeout = AbortSignal.timeout(duration);
    let combined = signal ? AbortSignal.any([signal, timeout]) : timeout;
    let response;
    try {
      response = await request(url.href, {
        method,
        headers: {
          Authorization: `Bearer ${auth.token}`,
          Accept: image
            ? "image/jpeg"
            : video
              ? "video/mp4"
              : "application/json",
        },
        redirect: "error",
        signal: combined,
      });
      if (response.status === 401 && auth.canRenew) {
        await response.body?.cancel().catch(() => undefined);
        await auth.renew();
        timeout = AbortSignal.timeout(
          Math.max(
            1,
            Math.min(
              requestTimeoutMs,
              deadline ? deadline - Date.now() : requestTimeoutMs,
            ),
          ),
        );
        combined = signal ? AbortSignal.any([signal, timeout]) : timeout;
        response = await request(url.href, {
          method,
          headers: {
            Authorization: `Bearer ${auth.token}`,
            Accept: image
              ? "image/jpeg"
              : video
                ? "video/mp4"
                : "application/json",
          },
          redirect: "error",
          signal: combined,
        });
      }
      if (response.status === 401 || response.status === 403)
        throw new ApiError(
          "Blink authorization failed. Renew the OAuth token and verify account access outside the collector.",
          "authentication_required",
        );
      if (!response.ok)
        throw new ApiError(
          response.status === 429
            ? "Blink rate limit reached; increase the task interval."
            : `Blink request failed with HTTP ${response.status}.`,
        );
      const bytes = await readBytes(
        response,
        image ? maximumImageBytes : video ? maximumClipBytes : 1_000_000,
      );
      if (image) {
        if (!completeJpeg(bytes))
          throw new ApiError("Blink did not return a complete JPEG thumbnail.");
        return bytes;
      }
      if (video) {
        if (
          bytes.length < 12 ||
          bytes.subarray(4, 8).toString("ascii") !== "ftyp"
        )
          throw new ApiError("Blink did not return an MP4 local clip.");
        return bytes;
      }
      try {
        return JSON.parse(bytes.toString("utf8"));
      } catch {
        throw new ApiError("Blink returned invalid JSON.");
      }
    } catch (error) {
      signal?.throwIfAborted();
      if (combined.aborted) throw new ApiError("Blink request timed out.");
      // Never propagate transport errors, which may contain URLs or authentication data.
      if (error instanceof ApiError) throw error;
      throw new ApiError("Blink network request failed.");
    }
  };
  const startedAt = Date.now();
  let homescreen = await api(`/api/v3/accounts/${config.accountId}/homescreen`);
  if (config.discoveryOnly) {
    const cameraInventory = [];
    for (const [type, group] of Object.entries(groups)) {
      for (const entry of Array.isArray(homescreen[group])
        ? homescreen[group]
        : []) {
        if (
          /^\d{1,20}$/.test(String(entry.id)) &&
          /^\d{1,20}$/.test(String(entry.network_id))
        )
          cameraInventory.push({
            id: String(entry.id),
            networkId: String(entry.network_id),
            type,
            name: String(entry.name ?? entry.id).slice(0, 200),
          });
      }
    }
    return {
      data: {
        accountId: config.accountId,
        region: config.region,
        cameraInventory,
      },
    };
  }
  const images = [],
    statuses = [],
    nextState = {},
    events = [];
  for (const camera of cameras) {
    const entries = homescreen[groups[camera.type]];
    const entry =
      Array.isArray(entries) &&
      entries.find(
        (item) =>
          String(item.id) === camera.id &&
          String(item.network_id) === camera.networkId,
      );
    camera.name = String(camera.name ?? entry?.name ?? camera.id).slice(0, 200);
  }
  let motion = { available: false, reason: "Motion polling is disabled." };
  if (config.motion !== false) {
    try {
      const polled = await motionPoll({
        api,
        cameras,
        config,
        state,
        startedAt,
      });
      nextState.motion = polled.state;
      events.push(...polled.events);
      motion = {
        available: true,
        source: "blink-cloud",
        baseline: !state?.motion?.checkedAt,
      };
    } catch (error) {
      signal?.throwIfAborted();
      if (state?.motion) nextState.motion = state.motion;
      motion = { available: false, reason: error.message };
    }
  }
  let localStorage,
    latestClips = {};
  if (config.localStorage === true) {
    try {
      const polled = await localStoragePoll({
        api,
        cameras,
        homescreen,
        config,
        state,
        startedAt,
        signal,
        timeoutMs: snapshotTimeoutMs,
      });
      nextState.localStorage = polled.state;
      latestClips = polled.latestClips;
      events.push(...polled.events);
      localStorage = {
        available: true,
        source: "blink-local-storage",
        clipCount: polled.clipCount,
        newClips: polled.events.length,
        baseline: !state?.localStorage,
      };
    } catch (error) {
      signal?.throwIfAborted();
      if (state?.localStorage) nextState.localStorage = state.localStorage;
      localStorage = { available: false, reason: error.message };
    }
  }
  const triggeredCameras = new Set(
    events.map((event) => event.payload.cameraId),
  );
  if (snapshotIntervalSeconds > 0 || useLocalClipImages)
    nextState.snapshots = {};
  let totalBytes = 0;
  for (const camera of cameras) {
    signal?.throwIfAborted();
    const snapshotKey = cameraKey(camera);
    const previousSnapshot = state?.snapshots?.[snapshotKey];
    if ((snapshotIntervalSeconds > 0 || useLocalClipImages) && previousSnapshot)
      nextState.snapshots[snapshotKey] = previousSnapshot;
    let fresh = false;
    try {
      let entry = cameraEntry(homescreen, camera);
      const previousSnapshotTime = Date.parse(previousSnapshot?.updatedAt);
      fresh =
        config.freshSnapshots !== false &&
        (snapshotIntervalSeconds === 0 ||
          !Number.isFinite(previousSnapshotTime) ||
          Date.now() - previousSnapshotTime >= snapshotIntervalSeconds * 1000 ||
          triggeredCameras.has(camera.id) ||
          previousSnapshot?.pending === true);
      if (fresh) {
        const oldThumbnail = String(entry.thumbnail ?? "");
        const deadline = Date.now() + snapshotTimeoutMs;
        const path =
          camera.type === "camera"
            ? `/network/${camera.networkId}/camera/${camera.id}/thumbnail`
            : `/api/v1/accounts/${config.accountId}/networks/${camera.networkId}/${groups[camera.type]}/${camera.id}/thumbnail`;
        const command = await api(path, { method: "POST", deadline });
        if (!/^\d+$/.test(String(command?.id ?? "")))
          throw new Error("Blink did not accept the snapshot command.");
        let complete = false;
        do {
          const status = await api(
            `/network/${camera.networkId}/command/${command.id}`,
            { deadline },
          );
          if (status.status_code !== 908)
            throw new Error(
              "Blink snapshot command failed or the camera is busy.",
            );
          complete = status.complete === true;
          if (!complete)
            await sleep(
              Math.max(1, Math.min(1000, deadline - Date.now())),
              undefined,
              { signal },
            );
        } while (!complete && Date.now() < deadline);
        if (!complete) throw new Error("Blink snapshot command timed out.");
        do {
          homescreen = await api(
            `/api/v3/accounts/${config.accountId}/homescreen`,
            { deadline },
          );
          entry = cameraEntry(homescreen, camera);
          if (String(entry.thumbnail ?? "") !== oldThumbnail && entry.thumbnail)
            break;
          if (Date.now() < deadline)
            await sleep(
              Math.max(1, Math.min(1000, deadline - Date.now())),
              undefined,
              { signal },
            );
        } while (Date.now() < deadline);
        if (!entry.thumbnail || String(entry.thumbnail) === oldThumbnail)
          throw new Error(
            "Blink snapshot completed but no fresh thumbnail became available.",
          );
      }
      const thumbnail = String(entry.thumbnail ?? "");
      const timestamp = /^\d{10}$/.test(thumbnail)
        ? thumbnail
        : new URL(thumbnail, base).searchParams.get("ts");
      const thumbnailCapturedAt = /^\d{10}$/.test(timestamp ?? "")
        ? new Date(Number(timestamp) * 1000).toISOString()
        : previousSnapshot?.thumbnail === thumbnail
          ? previousSnapshot.capturedAt
          : useLocalClipImages
            ? null
            : new Date().toISOString();
      let selected = thumbnail
        ? {
            capturedAt: thumbnailCapturedAt,
            source: "blink-thumbnail",
          }
        : null;
      let clipError;
      if (useLocalClipImages) {
        const path = join(
          cacheDirectory,
          ".frames",
          `${hash(`${config.accountId}:${snapshotKey}`)}.json`,
        );
        const cached = await cachedClipFrame(path, maximumImageBytes);
        if (
          cached &&
          (!Number.isFinite(Date.parse(selected?.capturedAt)) ||
            Date.parse(cached.capturedAt) > Date.parse(selected.capturedAt))
        )
          selected = cached;
        const clip = latestClips[snapshotKey];
        if (clip && cached?.clipKey !== localClipKey(clip)) {
          try {
            const downloaded = await downloadClipFrame({
              api,
              clip,
              config,
              options: clipOptions,
              path,
              signal,
              extractFrame,
            });
            if (
              !Number.isFinite(Date.parse(selected?.capturedAt)) ||
              Date.parse(downloaded.capturedAt) >
                Date.parse(selected.capturedAt)
            )
              selected = downloaded;
          } catch (error) {
            signal?.throwIfAborted();
            clipError =
              error instanceof ApiError
                ? error.message
                : "Saved Blink clip frame is unavailable.";
          }
        }
      }
      if (!selected)
        throw new ApiError("Blink has no saved image for this camera.");
      const bytes =
        selected.bytes ??
        (await api(thumbnailPath(entry, camera, config.accountId), {
          image: true,
        }));
      if (totalBytes + bytes.length > maximumTotalImageBytes)
        throw new Error(
          "Combined images exceed the result budget; use one task per camera.",
        );
      totalBytes += bytes.length;
      const capturedAt = selected.capturedAt;
      if (snapshotIntervalSeconds > 0 && fresh)
        nextState.snapshots[snapshotKey] = {
          updatedAt: new Date().toISOString(),
          thumbnail,
          capturedAt: thumbnailCapturedAt,
        };
      if (useLocalClipImages)
        nextState.snapshots[snapshotKey] = {
          ...previousSnapshot,
          thumbnail,
          capturedAt: thumbnailCapturedAt,
        };
      const source = useLocalClipImages
        ? {
            source: selected.source,
            ...(selected.clipId
              ? { clipId: selected.clipId, recordedAt: selected.recordedAt }
              : {}),
          }
        : {};
      images.push({
        id: camera.id,
        name: camera.name,
        dataUrl: `data:image/jpeg;base64,${bytes.toString("base64")}`,
        capturedAt,
        ...source,
      });
      statuses.push({
        id: camera.id,
        name: camera.name,
        status: "ok",
        fresh,
        capturedAt,
        ...source,
        ...(clipError ? { clipError } : {}),
      });
    } catch (error) {
      signal?.throwIfAborted();
      if (snapshotIntervalSeconds > 0 && fresh)
        nextState.snapshots[snapshotKey] = {
          ...previousSnapshot,
          pending: true,
        };
      statuses.push({
        id: camera.id,
        name: camera.name,
        status: "error",
        error: error.message,
        ...(error instanceof ApiError ? { errorCode: error.code } : {}),
      });
    }
  }
  if (images.length === 0 && !motion.available)
    throw new ApiError(
      statuses.map((camera) => `${camera.id}: ${camera.error}`).join("; "),
      statuses.find(
        (camera) =>
          camera.errorCode === "authentication_required" ||
          camera.errorCode === "interaction_required",
      )?.errorCode ?? "provider_unavailable",
    );
  return {
    data: {
      images,
      cameras: statuses,
      motion,
      ...(localStorage ? { localStorage } : {}),
    },
    state: nextState,
    events,
  };
}
