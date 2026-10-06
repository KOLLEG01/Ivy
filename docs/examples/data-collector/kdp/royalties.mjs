import { parse } from "parse5";
import { CookieJar } from "tough-cookie";
import ExcelJS from "exceljs";
import { createHash, createHmac, randomUUID } from "node:crypto";
import {
  lstat,
  readFile,
  open,
  rename,
  unlink,
  mkdir,
  chmod,
} from "node:fs/promises";

const reportsOrigin = "https://kdpreports.amazon.com";
const royaltiesPage = `${reportsOrigin}/reports/royalties`;
const cachePath = ".auth/kdp.json";
const amazonHosts = new Set([
  "kdpreports.amazon.com",
  "kdp.amazon.com",
  "www.amazon.com",
  "amazon.com",
]);
const fail = (code, message) => Object.assign(new Error(message), { code });

async function bytes(response, limit) {
  const chunks = [];
  let size = 0;
  for await (const chunk of response.body ?? []) {
    size += chunk.byteLength;
    if (size > limit)
      throw fail("provider_unavailable", "KDP response exceeds its limit");
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

function htmlInfo(html, url) {
  const forms = [];
  let challenge = false;
  let invalidCredentials = false;
  const textContent = (node) =>
    node.nodeName === "#text"
      ? node.value
      : (node.childNodes ?? []).map(textContent).join("");
  const promptText = (node) => {
    if (node.nodeName === "#text") return node.value;
    const attrs = Object.fromEntries(
      (node.attrs ?? []).map((a) => [a.name, a.value]),
    );
    // Links and controls can offer other MFA methods without selecting them.
    if (
      [
        "a",
        "button",
        "input",
        "select",
        "textarea",
        "script",
        "style",
      ].includes(node.tagName) ||
      "hidden" in attrs ||
      attrs["aria-hidden"] === "true"
    )
      return "";
    return (node.childNodes ?? []).map(promptText).join(" ");
  };
  function walk(node, form = null) {
    const attrs = Object.fromEntries(
      (node.attrs ?? []).map((a) => [a.name, a.value]),
    );
    if (/captcha|auth-error-message-box/i.test(attrs.id ?? "")) {
      if (/captcha/i.test(attrs.id)) challenge = true;
      else if (textContent(node).trim()) invalidCredentials = true;
    }
    if (node.tagName === "form") {
      let action;
      try {
        action = new URL(attrs.action || url, url);
      } catch {
        return;
      }
      form = {
        action,
        method: (attrs.method || "get").toUpperCase(),
        fields: new URLSearchParams(),
        names: new Set(),
        prompt: promptText(node),
      };
      forms.push(form);
      if (/validateCaptcha|\/challenge\b/i.test(action.pathname))
        challenge = true;
    }
    if (node.tagName === "input" && attrs.name && form) {
      form.names.add(attrs.name);
      if (/captcha|^guess$/i.test(attrs.name)) challenge = true;
      if (
        !("disabled" in attrs) &&
        !["checkbox", "radio", "button", "submit"].includes(attrs.type)
      )
        form.fields.append(attrs.name, attrs.value ?? "");
    }
    for (const child of node.childNodes ?? []) walk(child, form);
  }
  walk(parse(html));
  if (challenge)
    throw fail(
      "interaction_required",
      "Amazon requires a CAPTCHA or account confirmation",
    );
  if (invalidCredentials)
    throw fail(
      "authentication_required",
      "Amazon rejected the account credentials",
    );
  return forms;
}

function csrfFromHtml(html) {
  const match = html.match(
    /"csrftoken"\s*:\s*\{\s*"token"\s*:\s*("(?:\\.|[^"\\])*")/,
  );
  if (!match) return null;
  try {
    return JSON.parse(match[1]);
  } catch {
    throw fail("provider_unavailable", "KDP returned an invalid CSRF value");
  }
}

// Only the standard authenticator OTP form is supported, never an SMS/approval flow.
function totp(seed) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  const normalized = seed.replace(/[\s=]/g, "").toUpperCase();
  if (!normalized || !/^[A-Z2-7]+$/.test(normalized))
    throw fail("configuration_invalid", "Invalid Amazon TOTP seed");
  let bits = "";
  for (const c of normalized)
    bits += alphabet.indexOf(c).toString(2).padStart(5, "0");
  const key = Buffer.from(
    bits.match(/.{8}/g)?.map((b) => Number.parseInt(b, 2)) ?? [],
  );
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(Math.floor(Date.now() / 30_000)));
  const digest = createHmac("sha1", key).update(counter).digest();
  const offset = digest.at(-1) & 15;
  return String(
    (digest.readUInt32BE(offset) & 0x7fffffff) % 1_000_000,
  ).padStart(6, "0");
}

async function session(secrets, config, signal) {
  const cookie = secrets[config.cookieSecret ?? "kdp_cookie"] ?? "";
  const username = secrets[config.usernameSecret ?? "kdp_username"] ?? "";
  const password = secrets[config.passwordSecret ?? "kdp_password"] ?? "";
  const seed = secrets[config.totpSecret ?? "kdp_totp_seed"] ?? "";
  const fingerprint = createHash("sha256")
    .update(JSON.stringify([cookie, username, password, seed]))
    .digest("hex");
  let jar = new CookieJar();
  let cached = false;
  try {
    const directory = await lstat(".auth");
    if (!directory.isDirectory() || directory.isSymbolicLink())
      throw fail("configuration_invalid", "Invalid KDP session directory");
    const info = await lstat(cachePath);
    if (!info.isFile() || info.isSymbolicLink() || info.size > 200_000)
      throw fail("configuration_invalid", "Invalid KDP session cache");
    const saved = JSON.parse(await readFile(cachePath, "utf8"));
    if (saved.fingerprint === fingerprint) {
      jar = CookieJar.fromJSON(saved.jar);
      cached = true;
    }
  } catch (error) {
    if (error.code !== "ENOENT")
      throw fail(
        "configuration_invalid",
        "KDP session cache could not be read",
      );
  }
  if (cookie && !cached) {
    if (/[\r\n]/.test(cookie))
      throw fail("configuration_invalid", "Invalid assigned Amazon cookie");
    for (const entry of cookie.split(/;\s*/).filter(Boolean)) {
      if (!/^[^=\s;]+=/.test(entry))
        throw fail("configuration_invalid", "Invalid assigned Amazon cookie");
      try {
        await jar.setCookie(
          `${entry}; Domain=.amazon.com; Path=/; Secure`,
          royaltiesPage,
        );
      } catch {
        throw fail("configuration_invalid", "Invalid assigned Amazon cookie");
      }
    }
  }
  async function request(target, options = {}, binary = false) {
    let url;
    try {
      url = new URL(target);
    } catch {
      throw fail(
        "provider_unavailable",
        "KDP returned an invalid request destination",
      );
    }
    let method = options.method ?? "GET";
    let body = options.body;
    for (let redirect = 0; redirect <= 8; redirect++) {
      signal?.throwIfAborted();
      const amazon = amazonHosts.has(url.hostname);
      const storage =
        binary &&
        (url.hostname === "amazonaws.com" ||
          url.hostname.endsWith(".amazonaws.com"));
      if (
        url.protocol !== "https:" ||
        url.username ||
        url.password ||
        (!amazon && !storage)
      )
        throw fail(
          "provider_unavailable",
          "KDP returned an unexpected request destination",
        );
      const headers = amazon ? { ...options.headers } : {};
      const currentCookie = amazon ? await jar.getCookieString(url.href) : "";
      if (currentCookie) headers.Cookie = currentCookie;
      let response;
      try {
        response = await fetch(url, {
          method,
          body,
          headers,
          signal,
          redirect: "manual",
        });
      } catch {
        signal?.throwIfAborted();
        throw fail("provider_unavailable", "Amazon HTTP request failed");
      }
      if (amazon)
        for (const value of response.headers.getSetCookie()) {
          try {
            await jar.setCookie(value, url.href);
          } catch {
            throw fail(
              "provider_unavailable",
              "Amazon returned an invalid session cookie",
            );
          }
        }
      if ([301, 302, 303, 307, 308].includes(response.status)) {
        const location = response.headers.get("location");
        await response.body?.cancel();
        if (!location || redirect === 8)
          throw fail(
            "provider_unavailable",
            "Amazon redirects could not be resolved",
          );
        let next;
        try {
          next = new URL(location, url);
        } catch {
          throw fail(
            "provider_unavailable",
            "Amazon returned an invalid redirect",
          );
        }
        // Credentials/form bodies never cross an origin or repeat on a 307/308 redirect.
        if (method !== "GET" && [307, 308].includes(response.status))
          throw fail(
            "provider_unavailable",
            "Amazon returned an unsupported form redirect",
          );
        method = "GET";
        body = undefined;
        url = next;
        continue;
      }
      const content = await bytes(
        response,
        binary ? 16 * 1024 * 1024 : 2 * 1024 * 1024,
      );
      if (response.status === 401)
        throw fail("authentication_required", "Amazon session expired");
      if (response.status === 429)
        throw fail(
          "provider_unavailable",
          "Amazon rate limited the report request",
        );
      if (
        !binary ||
        /text\/html/i.test(response.headers.get("content-type") ?? "")
      )
        htmlInfo(content.toString("utf8"), url);
      if (!response.ok)
        throw fail(
          "provider_unavailable",
          `Amazon request failed (HTTP ${response.status})`,
        );
      return {
        url,
        content,
        contentType: response.headers.get("content-type") ?? "",
      };
    }
  }
  let page = await request(royaltiesPage);
  let csrf = csrfFromHtml(page.content.toString("utf8"));
  const submitted = new Set();
  for (let step = 0; !csrf && step < 3; step++) {
    if (!username || !password)
      throw fail(
        "authentication_required",
        "Assign an Amazon session or username/password",
      );
    const forms = htmlInfo(page.content.toString("utf8"), page.url);
    const form = forms.find((f) =>
      [...f.names].some((n) => ["email", "password", "otpCode"].includes(n)),
    );
    if (
      !form ||
      form.method !== "POST" ||
      !amazonHosts.has(form.action.hostname) ||
      form.action.protocol !== "https:" ||
      !/^\/ap\/(?:signin|mfa)(?:\/|$)/.test(form.action.pathname)
    )
      throw fail(
        "interaction_required",
        "Amazon requires an unsupported account confirmation",
      );
    const kind = form.names.has("otpCode")
      ? "otp"
      : form.names.has("password")
        ? "password"
        : "email";
    if (submitted.has(kind))
      throw fail(
        "authentication_required",
        "Amazon authentication did not complete",
      );
    submitted.add(kind);
    if (form.names.has("email")) form.fields.set("email", username);
    if (form.names.has("password")) form.fields.set("password", password);
    if (kind === "otp") {
      if (!seed)
        throw fail(
          "interaction_required",
          "Amazon requires an authenticator code",
        );
      const deviceType = form.fields.get("mfaDeviceType");
      const authenticator =
        deviceType !== null
          ? deviceType.trim().toUpperCase() === "TOTP"
          : /\b(?:authenticator|authentication app)\b/i.test(form.prompt) &&
            !/\b(?:sms|text message|approve|approval|phone number)\b/i.test(
              form.prompt,
            );
      if (!authenticator)
        throw fail(
          "interaction_required",
          "Amazon requires a different account confirmation",
        );
      form.fields.set("otpCode", totp(seed));
    }
    page = await request(form.action, {
      method: "POST",
      headers: {
        "Content-Type": "application/x-www-form-urlencoded",
        Origin: form.action.origin,
      },
      body: form.fields.toString(),
    });
    csrf = csrfFromHtml(page.content.toString("utf8"));
  }
  if (!csrf || page.url.origin !== reportsOrigin)
    throw fail(
      "authentication_required",
      "Amazon did not establish an authenticated KDP session",
    );
  async function save() {
    const temporary = `${cachePath}.${randomUUID()}.tmp`;
    try {
      await mkdir(".auth", { mode: 0o700 }).catch((error) => {
        if (error.code !== "EEXIST") throw error;
      });
      const directory = await lstat(".auth");
      if (!directory.isDirectory() || directory.isSymbolicLink())
        throw Error("Invalid session directory");
      await chmod(".auth", 0o700);
      const file = await open(temporary, "wx", 0o600);
      try {
        await file.writeFile(
          JSON.stringify({ fingerprint, jar: jar.serializeSync() }),
        );
        await file.sync();
      } finally {
        await file.close();
      }
      await rename(temporary, cachePath);
    } catch {
      await unlink(temporary).catch(() => {});
      throw fail(
        "provider_unavailable",
        "KDP session cache could not be saved",
      );
    }
  }
  await save();
  return { request, csrf, save };
}

const normalized = (value) =>
  String(value ?? "")
    .trim()
    .toLowerCase()
    .replace(/\s+/g, " ");
function cellValue(cell) {
  const value = cell?.value;
  if (value && typeof value === "object" && !(value instanceof Date)) {
    if ("result" in value) return value.result;
    if (Array.isArray(value.richText))
      return value.richText.map((part) => part.text).join("");
    if (typeof value.text === "string") return value.text;
  }
  return value;
}
function number(value) {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  let text = String(value ?? "")
    .trim()
    .replace(/\s/g, "");
  if (/^-?\d{1,3}(?:,\d{3})+(?:\.\d+)?$/.test(text))
    text = text.replace(/,/g, "");
  if (!/^-?\d+(?:\.\d+)?$/.test(text))
    throw fail("provider_unavailable", "KDP report contains an invalid amount");
  const parsed = Number(text);
  if (!Number.isFinite(parsed))
    throw fail("provider_unavailable", "KDP report contains an invalid amount");
  return parsed;
}
function reportDay(value) {
  if (value instanceof Date && Number.isFinite(value.getTime()))
    return value.toISOString().slice(0, 10);
  const text = String(value ?? "").trim();
  if (/^\d{4}-\d{2}-\d{2}(?:T|$)/.test(text)) return text.slice(0, 10);
  const match = text.match(/^(\d{1,2})\/(\d{1,2})\/(\d{4})$/);
  if (match)
    return `${match[3]}-${match[1].padStart(2, "0")}-${match[2].padStart(2, "0")}`;
  throw fail("provider_unavailable", "KDP report has an unsupported date");
}

export async function parseReport(buffer, period) {
  const workbook = new ExcelJS.Workbook();
  try {
    await workbook.xlsx.load(buffer);
  } catch {
    throw fail(
      "provider_unavailable",
      "KDP did not return a readable XLSX report",
    );
  }
  const sheets = [];
  for (const sheet of workbook.worksheets) {
    if (
      /^(?:(?:report\s+)?definitions?|(?:grand\s+)?totals?|summary|orders?)$/i.test(
        sheet.name,
      )
    )
      continue;
    if (!sheet.actualRowCount) continue;
    if (sheet.rowCount > 50_000)
      throw fail("provider_unavailable", "KDP report contains too many rows");
    let recognized = false;
    for (
      let rowIndex = 1;
      rowIndex <= Math.min(sheet.rowCount, 30);
      rowIndex++
    ) {
      const headers = sheet.getRow(rowIndex).values.map(normalized);
      const find = (...names) =>
        names.map((name) => headers.indexOf(name)).find((index) => index > 0) ??
        -1;
      const columns = {
        title: find("title", "book title"),
        asin: find("asin", "asin/isbn", "isbn"),
        currency: find("currency", "royalty currency"),
        royalty: find(
          "royalty",
          "estimated royalty",
          "estimated royalties",
          "net royalty",
        ),
        pages: find(
          "kenp read",
          "kindle edition normalized pages (kenp) read",
          "kindle edition normalized page (kenp) read",
          "kenp pages read",
        ),
        type: find("transaction type", "royalty type"),
        net: find("net units sold", "net units"),
        sold: find("units sold"),
        refunded: find("units refunded"),
        date: find("royalty date", "date", "sales date", "order date"),
      };
      if (
        columns.title > 0 &&
        columns.asin > 0 &&
        (columns.royalty > 0 || columns.pages > 0)
      ) {
        sheets.push({ sheet, rowIndex, columns });
        recognized = true;
        break;
      }
    }
    if (!recognized)
      throw fail(
        "provider_unavailable",
        "KDP report contains an unrecognized sheet",
      );
  }
  const hasDetailRows = ({ sheet, rowIndex, columns }) => {
    for (let index = rowIndex + 1; index <= sheet.rowCount; index++) {
      const row = sheet.getRow(index);
      const title = String(cellValue(row.getCell(columns.title)) ?? "").trim();
      const asin = String(cellValue(row.getCell(columns.asin)) ?? "").trim();
      if (!asin && /^(?:grand )?total(?:s)?\b/i.test(title)) continue;
      if (row.values.some((value) => String(value ?? "").trim())) return true;
    }
    return false;
  };
  for (const source of sheets) {
    if (!hasDetailRows(source)) continue;
    if (
      !/kenp|kindle.*normalized/i.test(source.sheet.name) &&
      source.columns.royalty < 0
    )
      throw fail(
        "provider_unavailable",
        "KDP report contains an unsupported detail sheet",
      );
    if (source.columns.royalty > 0 && source.columns.currency < 0)
      throw fail(
        "provider_unavailable",
        "KDP detail sheet is missing a royalty currency",
      );
    if (
      /kenp|kindle.*normalized/i.test(source.sheet.name) &&
      source.columns.pages < 0
    )
      throw fail(
        "provider_unavailable",
        "KDP Kindle subscription sheet is missing its page counts",
      );
  }
  const formatOf = (source) =>
    source.sheet.name
      .match(
        /^(ebook|paperback|hardcover|audio\s*book)\s+(?:royalty|royalties|sales|earnings)$/i,
      )?.[1]
      .toLowerCase()
      .replace(/\s/g, "");
  const sales = sheets.filter(
    (s) =>
      s.columns.royalty > 0 &&
      s.columns.currency > 0 &&
      !/kenp|kindle.*normalized/i.test(s.sheet.name),
  );
  const combined = sales.filter((s) => /combined/i.test(s.sheet.name));
  let selected;
  if (combined.length === 1) {
    selected = combined;
    if (
      sales.some(
        (source) =>
          !selected.includes(source) &&
          !formatOf(source) &&
          hasDetailRows(source),
      )
    )
      throw fail(
        "provider_unavailable",
        "KDP report contains an unsupported royalty detail sheet",
      );
  } else if (combined.length > 1)
    throw fail(
      "provider_unavailable",
      "KDP report has ambiguous combined sheets",
    );
  else {
    const detailed = sales.filter((s) => formatOf(s));
    const formats = detailed.map(formatOf);
    if (detailed.length && new Set(formats).size === formats.length) {
      selected = detailed;
      if (
        sales.some(
          (source) => !selected.includes(source) && hasDetailRows(source),
        )
      )
        throw fail(
          "provider_unavailable",
          "KDP report contains an unsupported royalty detail sheet",
        );
    } else if (!detailed.length && sales.length === 1) selected = sales;
    else
      throw fail(
        "provider_unavailable",
        "KDP royalty detail sheets are unavailable or ambiguous",
      );
  }
  const kenp = sheets.filter(
    (s) => /kenp|kindle.*normalized/i.test(s.sheet.name) && s.columns.pages > 0,
  );
  if (kenp.length > 1)
    throw fail(
      "provider_unavailable",
      "KDP report has ambiguous Kindle Unlimited sheets",
    );
  const totals = new Map();
  const embeddedKuTotals = new Map();
  let salesRows = 0,
    kuRows = 0,
    pagesRead = null,
    embeddedKu = false;
  const total = (currency) => {
    if (!/^[A-Z]{3}$/.test(currency))
      throw fail(
        "provider_unavailable",
        "KDP report is missing a royalty currency",
      );
    if (!totals.has(currency))
      totals.set(currency, {
        currency,
        salesEstimatedRoyalties: 0,
        subscriptionEstimatedRoyalties: 0,
        netUnitsSold: 0,
        netUnitsComplete: true,
      });
    return totals.get(currency);
  };
  function rows(source, read) {
    const { sheet, rowIndex, columns: c } = source;
    for (let index = rowIndex + 1; index <= sheet.rowCount; index++) {
      const row = sheet.getRow(index);
      const value = (key) =>
        c[key] > 0 ? cellValue(row.getCell(c[key])) : null;
      const asin = String(value("asin") ?? "").trim();
      const title = String(value("title") ?? "").trim();
      if (!asin) {
        if (/^(?:grand )?total(?:s)?\b/i.test(title)) continue;
        if (
          !title &&
          ["royalty", "pages", "currency"].every(
            (key) => value(key) == null || value(key) === "",
          )
        )
          continue;
        throw fail(
          "provider_unavailable",
          "KDP detail row is missing its book identifier",
        );
      }
      if (c.date > 0) {
        const day = reportDay(value("date"));
        if (
          day < period.startDate.slice(0, 10) ||
          day > period.endDate.slice(0, 10)
        )
          throw fail(
            "provider_unavailable",
            "KDP returned rows outside the requested month",
          );
      }
      read(value, c);
    }
  }
  for (const source of selected)
    rows(source, (value, c) => {
      const bucket = total(
        String(value("currency") ?? "")
          .trim()
          .toUpperCase(),
      );
      const royalty = number(value("royalty"));
      if (
        /kenp|kindle unlimited|normalized page|audible subscription|audible plus/i.test(
          String(value("type") ?? ""),
        )
      ) {
        bucket.subscriptionEstimatedRoyalties += royalty;
        embeddedKuTotals.set(
          bucket.currency,
          (embeddedKuTotals.get(bucket.currency) ?? 0) + royalty,
        );
        kuRows++;
        embeddedKu = true;
      } else {
        bucket.salesEstimatedRoyalties += royalty;
        salesRows++;
        if (c.net > 0) bucket.netUnitsSold += number(value("net"));
        else if (c.sold > 0 && c.refunded > 0)
          bucket.netUnitsSold +=
            number(value("sold")) - number(value("refunded"));
        else bucket.netUnitsComplete = false;
      }
    });
  if (
    !salesRows &&
    !kuRows &&
    sales.some(
      (source) =>
        !selected.includes(source) &&
        Array.from(
          { length: Math.max(0, source.sheet.rowCount - source.rowIndex) },
          (_, index) =>
            cellValue(
              source.sheet
                .getRow(source.rowIndex + index + 1)
                .getCell(source.columns.asin),
            ),
        ).some((value) => String(value ?? "").trim()),
    )
  )
    throw fail(
      "provider_unavailable",
      "KDP combined sheet is empty while detail rows exist",
    );
  const kuSheet = kenp[0];
  const kuSheetHasRoyalties = Boolean(
    kuSheet && kuSheet.columns.royalty > 0 && kuSheet.columns.currency > 0,
  );
  const kuRoyaltiesAvailable = embeddedKu || kuSheetHasRoyalties;
  const kuSheetTotals = new Map();
  if (kuSheet) {
    pagesRead = 0;
    rows(kuSheet, (value) => {
      const pages = number(value("pages"));
      if (!Number.isInteger(pages) || pages < 0)
        throw fail(
          "provider_unavailable",
          "KDP returned an invalid KENP count",
        );
      pagesRead += pages;
      if (kuSheetHasRoyalties) {
        const bucket = total(
          String(value("currency") ?? "")
            .trim()
            .toUpperCase(),
        );
        const royalty = number(value("royalty"));
        kuSheetTotals.set(
          bucket.currency,
          (kuSheetTotals.get(bucket.currency) ?? 0) + royalty,
        );
        if (!embeddedKu) {
          bucket.subscriptionEstimatedRoyalties += royalty;
          kuRows++;
        }
      }
    });
  }
  if (!selected.length)
    throw fail(
      "provider_unavailable",
      "KDP returned no recognizable royalty detail",
    );
  const round = (value) =>
    Math.round((value + Number.EPSILON) * 1_000_000) / 1_000_000;
  if (embeddedKu && kuSheetHasRoyalties)
    for (const currency of new Set([
      ...embeddedKuTotals.keys(),
      ...kuSheetTotals.keys(),
    ]))
      if (
        round(embeddedKuTotals.get(currency) ?? 0) !==
        round(kuSheetTotals.get(currency) ?? 0)
      )
        throw fail(
          "provider_unavailable",
          "KDP Kindle subscription royalty sheets disagree",
        );
  return {
    basis: "estimated_royalties",
    period,
    salesRows,
    subscriptionRows: kuRows,
    sheetsUsed: selected.map((s) => s.sheet.name),
    subscriptionSheet: kuSheet?.sheet.name ?? null,
    subscriptions: {
      status: kuRoyaltiesAvailable
        ? "estimated_royalties"
        : kuSheet
          ? "pages_only"
          : "not_in_report",
      pagesRead,
    },
    royaltiesByCurrency: [...totals.values()]
      .sort((a, b) => a.currency.localeCompare(b.currency))
      .map((t) => ({
        currency: t.currency,
        salesEstimatedRoyalties: round(t.salesEstimatedRoyalties),
        subscriptionEstimatedRoyalties: kuRoyaltiesAvailable
          ? round(t.subscriptionEstimatedRoyalties)
          : null,
        combinedEstimatedRoyalties: kuRoyaltiesAvailable
          ? round(t.salesEstimatedRoyalties + t.subscriptionEstimatedRoyalties)
          : null,
        netUnitsSold: t.netUnitsComplete ? t.netUnitsSold : null,
      })),
  };
}

export default async function collect({ config, secrets, signal }) {
  if (
    !secrets[config.cookieSecret ?? "kdp_cookie"] &&
    (!secrets[config.usernameSecret ?? "kdp_username"] ||
      !secrets[config.passwordSecret ?? "kdp_password"])
  )
    throw fail(
      "configuration_invalid",
      "Assign a KDP session or Amazon username/password",
    );
  const now = new Date();
  const period = {
    startDate: new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1))
      .toISOString()
      .replace(".000Z", "Z"),
    endDate: new Date(
      Date.UTC(
        now.getUTCFullYear(),
        now.getUTCMonth(),
        now.getUTCDate(),
        23,
        59,
        59,
      ),
    )
      .toISOString()
      .replace(".000Z", "Z"),
    timezone: "UTC",
  };
  const auth = await session(secrets, config, signal);
  const headers = {
    "Content-Type": "application/json",
    Accept: "application/json",
    "X-Csrf-Token": auth.csrf,
    "X-Requested-With": "XMLHttpRequest",
    Origin: reportsOrigin,
    Referer: royaltiesPage,
  };
  async function json(path, options) {
    const response = await auth.request(new URL(path, reportsOrigin), {
      ...options,
      headers,
    });
    if (
      response.url.origin !== reportsOrigin ||
      /\/ap\//.test(response.url.pathname)
    )
      throw fail(
        "authentication_required",
        "Amazon session expired during report collection",
      );
    try {
      return JSON.parse(response.content.toString("utf8"));
    } catch {
      throw fail(
        "provider_unavailable",
        "KDP returned an unsupported report response",
      );
    }
  }
  const metadata = await json("/api/v2/reports/booksMetadata");
  if (!Array.isArray(metadata?.Books))
    throw fail("provider_unavailable", "KDP books metadata is unsupported");
  const asins = metadata.Books.map((book) => book.ASIN);
  if (
    asins.some(
      (asin) => typeof asin !== "string" || !/^[A-Z0-9]{10}$/.test(asin),
    )
  )
    throw fail("provider_unavailable", "KDP returned invalid book identifiers");
  const initialized = await auth.request(
    new URL(
      `/api/v2/reports/pagesReadByAsin?${new URLSearchParams({ startDate: period.startDate, endDate: period.endDate, granularity: "DAY", asins: asins.join(",") })}`,
      reportsOrigin,
    ),
    { headers },
  );
  if (initialized.url.origin !== reportsOrigin)
    throw fail(
      "authentication_required",
      "Amazon session expired during report initialization",
    );
  const download = await json(
    "/download/report/royaltiesestimator/en_US/royaltiesEstimatorReport.xslx",
    {
      method: "POST",
      body: JSON.stringify({
        asins: null,
        authors: null,
        distribution: null,
        formats: null,
        marketplaces: null,
        reportStartDate: period.startDate,
        reportEndDate: period.endDate,
        reportGranularity: "DAY",
        reportType: "royalties",
      }),
    },
  );
  if (typeof download.url !== "string")
    throw fail("provider_unavailable", "KDP returned no report download URL");
  const report = await auth.request(download.url, {}, true);
  const data = await parseReport(report.content, period);
  await auth.save();
  return {
    data: {
      ...data,
      observedAt: now.toISOString(),
      source: "kdp-royalties-estimator",
    },
    state: {
      month: period.startDate.slice(0, 7),
      reportSha256: createHash("sha256").update(report.content).digest("hex"),
    },
  };
}
