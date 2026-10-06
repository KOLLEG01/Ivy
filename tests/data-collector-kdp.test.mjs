import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ExcelJS from "exceljs";
import collect, {
  parseReport,
} from "../docs/examples/data-collector/kdp/royalties.mjs";

const period = {
  startDate: "2026-09-01T00:00:00Z",
  endDate: "2026-09-26T23:59:59Z",
  timezone: "UTC",
};
const salesHeaders = [
  "Royalty Date",
  "Title",
  "ASIN",
  "Marketplace",
  "Royalty Type",
  "Transaction Type",
  "Net Units Sold",
  "Currency",
  "Royalty",
];
const salesRow = (currency = "USD", royalty = 3.75, units = 2) => [
  "09/10/2026",
  "Fixture book",
  "B000000001",
  "Amazon.com",
  "70%",
  "Standard",
  units,
  currency,
  royalty,
];
const kenpHeaders = [
  "Royalty Date",
  "Title",
  "ASIN",
  "KENP Read",
  "Currency",
  "Royalty",
];
const kenpRow = (royalty = 0.5) => [
  "09/10/2026",
  "Fixture book",
  "B000000001",
  100,
  "USD",
  royalty,
];

async function workbook(sheets) {
  const book = new ExcelJS.Workbook();
  for (const [name, rows] of Object.entries(sheets))
    book.addWorksheet(name).addRows(rows);
  return Buffer.from(await book.xlsx.writeBuffer());
}

async function isolated(t) {
  const previous = process.cwd();
  const directory = await mkdtemp(join(tmpdir(), "data-collector-kdp-"));
  process.chdir(directory);
  t.after(async () => {
    process.chdir(previous);
    await rm(directory, { recursive: true, force: true });
  });
  t.mock.timers.enable({
    apis: ["Date"],
    now: new Date("2026-09-26T12:00:00Z"),
  });
  return directory;
}

const authenticatedPage =
  '<script>window.data={"csrftoken":{"token":"fixture-csrf-private"}};</script>';
const captchaPage =
  '<form action="/errors_page/validateCaptcha"><input name="amzn"><input name="guess" id="captchacharacters"></form>';
const form = (fields, action = "/ap/signin", text = "") =>
  '<form method="POST" action="' +
  action +
  '">' +
  text +
  fields
    .map(
      ([name, value = ""]) =>
        '<input name="' + name + '" value="' + value + '">',
    )
    .join("") +
  "</form>";
const redirect = (url, headers = {}) =>
  new Response(null, { status: 302, headers: { location: url, ...headers } });
const html = (value) =>
  new Response(value, { headers: { "content-type": "text/html" } });
const json = (value) =>
  new Response(JSON.stringify(value), {
    headers: { "content-type": "application/json" },
  });

function reportFetch(t, report, before) {
  const requests = [];
  t.mock.method(globalThis, "fetch", async (target, options = {}) => {
    const url = new URL(target);
    const request = { url, options, headers: new Headers(options.headers) };
    requests.push(request);
    const response = await before?.(request);
    if (response) return response;
    if (url.origin === "https://kdpreports.amazon.com") {
      if (url.pathname === "/reports/royalties") return html(authenticatedPage);
      assert.equal(request.headers.get("X-Csrf-Token"), "fixture-csrf-private");
      if (url.pathname === "/api/v2/reports/booksMetadata")
        return json({ Books: [{ ASIN: "B000000001" }] });
      if (url.pathname === "/api/v2/reports/pagesReadByAsin") {
        assert.equal(url.searchParams.get("asins"), "B000000001");
        assert.equal(url.searchParams.get("startDate"), period.startDate);
        assert.equal(url.searchParams.get("endDate"), period.endDate);
        assert.equal(url.searchParams.get("granularity"), "DAY");
        return new Response(null, { status: 204 });
      }
      if (
        url.pathname ===
        "/download/report/royaltiesestimator/en_US/royaltiesEstimatorReport.xslx"
      ) {
        assert.equal(options.method, "POST");
        assert.deepEqual(JSON.parse(options.body), {
          asins: null,
          authors: null,
          distribution: null,
          formats: null,
          marketplaces: null,
          reportStartDate: period.startDate,
          reportEndDate: period.endDate,
          reportGranularity: "DAY",
          reportType: "royalties",
        });
        return json({
          url: "https://fixture-kdp.s3.amazonaws.com/report.xlsx?signature=fixture-private-signature",
        });
      }
    }
    if (url.hostname === "fixture-kdp.s3.amazonaws.com") {
      assert.equal(request.headers.get("cookie"), null);
      assert.equal(request.headers.get("X-Csrf-Token"), null);
      assert.equal(options.body, undefined);
      return new Response(report, {
        headers: {
          "content-type":
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        },
      });
    }
    assert.fail("Unexpected fixture request: " + url.origin + url.pathname);
  });
  return requests;
}

test("Combined Sales avoids overlapping detail, subtotals and cross-currency summation", async () => {
  const report = await workbook({
    "Combined Sales": [
      salesHeaders,
      salesRow(),
      salesRow("EUR", 2.5, 1),
      ["", "Grand Total", "", "", "", "", "", "USD", 999],
    ],
    "eBook Royalties": [salesHeaders, salesRow("USD", 1000)],
    "Paperback Royalties": [salesHeaders, salesRow("USD", 1000)],
    Summary: [salesHeaders, salesRow("USD", 1000)],
    "KENP Read": [
      ["Royalty Date", "Title", "ASIN", "KENP Read"],
      ["09/10/2026", "Fixture book", "B000000001", 16],
    ],
  });
  const result = await parseReport(report, period);
  assert.deepEqual(result.sheetsUsed, ["Combined Sales"]);
  assert.equal(result.salesRows, 2);
  assert.deepEqual(result.subscriptions, {
    status: "pages_only",
    pagesRead: 16,
  });
  assert.deepEqual(result.royaltiesByCurrency, [
    {
      currency: "EUR",
      salesEstimatedRoyalties: 2.5,
      subscriptionEstimatedRoyalties: null,
      combinedEstimatedRoyalties: null,
      netUnitsSold: 1,
    },
    {
      currency: "USD",
      salesEstimatedRoyalties: 3.75,
      subscriptionEstimatedRoyalties: null,
      combinedEstimatedRoyalties: null,
      netUnitsSold: 2,
    },
  ]);
});

test("disjoint format details retain refunds and actual KU royalties", async () => {
  const report = await workbook({
    "eBook Royalties": [
      salesHeaders,
      salesRow("USD", 3, 2),
      salesRow("USD", -1, -1),
    ],
    "Hardcover Royalties": [salesHeaders, salesRow("EUR", 4, 1)],
    "KENP Read": [kenpHeaders, kenpRow()],
  });
  const result = await parseReport(report, period);
  assert.deepEqual(result.sheetsUsed, [
    "eBook Royalties",
    "Hardcover Royalties",
  ]);
  assert.deepEqual(result.subscriptions, {
    status: "estimated_royalties",
    pagesRead: 100,
  });
  assert.equal(result.subscriptionRows, 1);
  assert.deepEqual(result.royaltiesByCurrency, [
    {
      currency: "EUR",
      salesEstimatedRoyalties: 4,
      subscriptionEstimatedRoyalties: 0,
      combinedEstimatedRoyalties: 4,
      netUnitsSold: 1,
    },
    {
      currency: "USD",
      salesEstimatedRoyalties: 2,
      subscriptionEstimatedRoyalties: 0.5,
      combinedEstimatedRoyalties: 2.5,
      netUnitsSold: 1,
    },
  ]);
});

test("verified audiobook royalty detail is included alongside other disjoint formats", async () => {
  const result = await parseReport(
    await workbook({
      "eBook Royalties": [salesHeaders, salesRow("USD", 3, 1)],
      "Audiobook Royalties": [salesHeaders, salesRow("USD", 7, 2)],
    }),
    period,
  );
  assert.deepEqual(result.sheetsUsed, [
    "eBook Royalties",
    "Audiobook Royalties",
  ]);
  assert.equal(result.salesRows, 2);
  assert.equal(result.royaltiesByCurrency[0].salesEstimatedRoyalties, 10);
  assert.equal(result.royaltiesByCurrency[0].netUnitsSold, 3);
});

test("populated unsupported or currency-less detail never produces partial totals", async () => {
  const withoutCurrency = (values, index) =>
    values.filter((_, i) => i !== index);
  const currencyLessSales = [
    withoutCurrency(salesHeaders, 7),
    withoutCurrency(salesRow(), 7),
  ];
  const currencyLessKenp = [
    withoutCurrency(kenpHeaders, 4),
    withoutCurrency(kenpRow(), 4),
  ];
  const cases = [
    {
      "eBook Royalties": [salesHeaders, salesRow()],
      "Paperback Royalties": currencyLessSales,
    },
    {
      "Combined Sales": [salesHeaders, salesRow()],
      "Paperback Royalties": currencyLessSales,
    },
    {
      "eBook Royalties": [salesHeaders, salesRow()],
      "New Format Royalties": [salesHeaders, salesRow("USD", 9)],
    },
    {
      "Combined Sales": [salesHeaders, salesRow()],
      "New Format Royalties": [salesHeaders, salesRow("USD", 9)],
    },
    {
      "eBook Royalties": [salesHeaders, salesRow()],
      "New Format Royalties": [
        ["Book", "Identifier", "Amount"],
        ["Fixture book", "B000000001", 9],
      ],
    },
    {
      "Combined Sales": [salesHeaders, salesRow()],
      "KENP Read": currencyLessKenp,
    },
    {
      "Combined Sales": [salesHeaders, salesRow()],
      "KENP Read": [
        kenpHeaders.filter((_, i) => i !== 3),
        kenpRow().filter((_, i) => i !== 3),
      ],
    },
    {
      "Combined Sales": [salesHeaders, salesRow()],
      "New Subscription Detail": [
        ["Royalty Date", "Title", "ASIN", "KENP Read"],
        ["09/10/2026", "Fixture book", "B000000001", 100],
      ],
    },
  ];
  for (const sheets of cases)
    await assert.rejects(parseReport(await workbook(sheets), period), {
      code: "provider_unavailable",
    });
});

test("an empty unused format sheet does not make complete recognized detail unavailable", async () => {
  const result = await parseReport(
    await workbook({
      "eBook Royalties": [salesHeaders, salesRow()],
      "Paperback Royalties": [salesHeaders.filter((_, i) => i !== 7)],
    }),
    period,
  );
  assert.deepEqual(result.sheetsUsed, ["eBook Royalties"]);
  assert.equal(result.royaltiesByCurrency[0].salesEstimatedRoyalties, 3.75);
});

test("KU and Audible subscriptions in Combined Sales are not added twice from KENP", async () => {
  for (const type of [
    "Kindle Unlimited",
    "Audible subscription royalties",
    "Audible Plus",
  ]) {
    const subscription = salesRow("USD", 0.5, 0);
    subscription[5] = type;
    const result = await parseReport(
      await workbook({
        "Combined Sales": [salesHeaders, salesRow("USD", 3, 1), subscription],
        "KENP Read": [kenpHeaders, kenpRow()],
      }),
      period,
    );
    assert.equal(result.salesRows, 1, type);
    assert.equal(result.subscriptionRows, 1, type);
    assert.equal(
      result.royaltiesByCurrency[0].salesEstimatedRoyalties,
      3,
      type,
    );
    assert.equal(
      result.royaltiesByCurrency[0].subscriptionEstimatedRoyalties,
      0.5,
      type,
    );
    assert.equal(
      result.royaltiesByCurrency[0].combinedEstimatedRoyalties,
      3.5,
      type,
    );
    assert.equal(result.subscriptions.pagesRead, 100, type);
  }
});

test("embedded KU only suppresses KENP amounts with matching currency totals", async () => {
  const ku = salesRow("USD", 0.5, 0);
  ku[5] = "Kindle Unlimited";
  const extraCurrency = kenpRow(1);
  extraCurrency[4] = "EUR";
  for (const kenpRows of [[kenpRow(1)], [kenpRow(), extraCurrency], []])
    await assert.rejects(
      parseReport(
        await workbook({
          "Combined Sales": [
            salesHeaders,
            salesRow(),
            salesRow("EUR", 4, 1),
            ku,
          ],
          "KENP Read": [kenpHeaders, ...kenpRows],
        }),
        period,
      ),
      { code: "provider_unavailable" },
    );
  const matched = await parseReport(
    await workbook({
      "Combined Sales": [salesHeaders, salesRow("USD", 3, 1), ku],
      "KENP Read": [kenpHeaders, kenpRow(0.2), kenpRow(0.3)],
    }),
    period,
  );
  assert.equal(matched.royaltiesByCurrency[0].combinedEstimatedRoyalties, 3.5);
  assert.equal(matched.subscriptionRows, 1);
  assert.equal(matched.subscriptions.pagesRead, 200);
});

test("missing KU coverage is explicit, not a synthetic zero", async () => {
  const result = await parseReport(
    await workbook({ "Combined Sales": [salesHeaders, salesRow()] }),
    period,
  );
  assert.deepEqual(result.subscriptions, {
    status: "not_in_report",
    pagesRead: null,
  });
  assert.equal(
    result.royaltiesByCurrency[0].subscriptionEstimatedRoyalties,
    null,
  );
  assert.equal(result.royaltiesByCurrency[0].combinedEstimatedRoyalties, null);
});

test("unreadable, ambiguous, stale or malformed workbooks fail instead of publishing totals", async () => {
  const broken = salesRow("USD", "unknown");
  const stale = salesRow();
  stale[0] = "08/31/2026";
  const missing = salesRow();
  missing[2] = "";
  const cases = [
    Buffer.from("not an XLSX file"),
    await workbook({ "Combined Sales": [salesHeaders, salesRow("", 3)] }),
    await workbook({ "Combined Sales": [salesHeaders, broken] }),
    await workbook({ "Combined Sales": [salesHeaders, stale] }),
    await workbook({ "Combined Sales": [salesHeaders, missing] }),
    await workbook({
      "eBook Royalties": [salesHeaders, salesRow()],
      "eBook Sales": [salesHeaders, salesRow()],
    }),
    await workbook({ Summary: [salesHeaders, salesRow()] }),
    await workbook({
      "Combined Sales": [salesHeaders],
      "eBook Royalties": [salesHeaders, salesRow()],
    }),
  ];
  for (const value of cases)
    await assert.rejects(parseReport(value, period), {
      code: "provider_unavailable",
    });
});

test("ordinary email/password/authenticator forms preserve hidden fields and download XLSX", async (t) => {
  const directory = await isolated(t);
  const report = await workbook({
    "Combined Sales": [salesHeaders, salesRow()],
  });
  let authenticated = false;
  let phase = "email";
  const requests = reportFetch(t, report, ({ url, options }) => {
    if (url.pathname === "/reports/royalties" && !authenticated)
      return redirect("https://www.amazon.com/ap/signin");
    if (url.pathname === "/ap/signin" && (options.method ?? "GET") === "GET")
      return html(
        form([["appActionToken", "fixture-action-private"], ["email"]]),
      );
    if (url.pathname === "/ap/signin") {
      const body = new URLSearchParams(options.body);
      assert.equal(options.method, "POST");
      assert.equal(body.get("appActionToken"), "fixture-action-private");
      if (phase === "email") {
        assert.equal(body.get("email"), "fixture@example.invalid");
        phase = "password";
        return html(
          form([["appActionToken", "fixture-action-private"], ["password"]]),
        );
      }
      assert.equal(body.get("password"), "fixture-password-private");
      phase = "otp";
      return html(
        form(
          [
            ["mfaToken", "fixture-mfa-private"],
            ["mfaDeviceType", "TOTP"],
            ["otpCode"],
          ],
          "/ap/mfa",
          'Enter your verification code. SMS is available as an alternative. <a href="/ap/mfa?method=sms">Send an SMS instead</a>',
        ) +
          "<aside>Help with phone number changes and approval requests</aside>",
      );
    }
    if (url.pathname === "/ap/mfa") {
      const body = new URLSearchParams(options.body);
      assert.equal(body.get("mfaToken"), "fixture-mfa-private");
      assert.equal(body.get("mfaDeviceType"), "TOTP");
      assert.match(body.get("otpCode"), /^\d{6}$/);
      authenticated = true;
      return redirect("https://kdpreports.amazon.com/reports/royalties", {
        "set-cookie":
          "session=fixture-auth-session; Domain=.amazon.com; Path=/; Secure; HttpOnly",
      });
    }
  });
  const result = await collect({
    config: {},
    secrets: {
      kdp_username: "fixture@example.invalid",
      kdp_password: "fixture-password-private",
      kdp_totp_seed: "JBSWY3DPEHPK3PXP",
    },
    signal: new AbortController().signal,
  });
  assert.equal(
    result.data.royaltiesByCurrency[0].salesEstimatedRoyalties,
    3.75,
  );
  assert.equal(result.data.observedAt, "2026-09-26T12:00:00.000Z");
  assert.deepEqual(result.state, {
    month: "2026-09",
    reportSha256: result.state.reportSha256,
  });
  assert.match(result.state.reportSha256, /^[a-f0-9]{64}$/);
  assert.equal(
    requests.filter(
      (r) => r.options.method === "POST" && r.url.hostname === "www.amazon.com",
    ).length,
    3,
  );
  for (const secret of [
    "fixture@example.invalid",
    "fixture-password-private",
    "fixture-auth-session",
    "fixture-csrf-private",
    "fixture-private-signature",
  ])
    assert.equal(JSON.stringify(result).includes(secret), false);
  const saved = JSON.parse(
    await readFile(join(directory, ".auth/kdp.json"), "utf8"),
  );
  assert.match(saved.fingerprint, /^[a-f0-9]{64}$/);
  assert.equal(
    saved.jar.cookies.some((c) => c.value === "fixture-auth-session"),
    true,
  );
  assert.deepEqual(await readdir(join(directory, ".auth")), ["kdp.json"]);
  if (process.platform !== "win32") {
    assert.equal((await stat(join(directory, ".auth"))).mode & 0o777, 0o700);
    assert.equal(
      (await stat(join(directory, ".auth/kdp.json"))).mode & 0o777,
      0o600,
    );
  }
});

test("assigned cookie is preferred, cache is reused and credential changes invalidate it", async (t) => {
  const directory = await isolated(t);
  const report = await workbook({
    "Combined Sales": [salesHeaders, salesRow()],
  });
  let generation = 1;
  reportFetch(t, report, ({ url, headers }) => {
    if (url.pathname !== "/reports/royalties") return;
    if (generation === 1) {
      assert.equal(headers.get("Cookie"), "session=fixture-assigned");
      return new Response(authenticatedPage, {
        headers: {
          "content-type": "text/html",
          "set-cookie":
            "session=fixture-refreshed; Domain=.amazon.com; Path=/; Secure; HttpOnly",
        },
      });
    }
    if (generation === 2) {
      assert.equal(headers.get("Cookie"), "session=fixture-refreshed");
      return html(authenticatedPage);
    }
    assert.equal(headers.get("Cookie"), "session=fixture-assigned");
    return html(captchaPage);
  });
  const context = {
    config: {},
    secrets: {
      kdp_cookie: "session=fixture-assigned",
      kdp_username: "fixture@example.invalid",
      kdp_password: "fixture-password-private",
    },
  };
  await collect(context);
  const initial = await readFile(join(directory, ".auth/kdp.json"), "utf8");
  generation = 2;
  await collect(context);
  generation = 3;
  await assert.rejects(
    collect({
      ...context,
      secrets: { ...context.secrets, kdp_password: "changed-fixture-password" },
    }),
    { code: "interaction_required" },
  );
  assert.equal(
    await readFile(join(directory, ".auth/kdp.json"), "utf8"),
    initial,
  );
});

test("CAPTCHA is reported before sending credentials and never returns old observations", async (t) => {
  const directory = await isolated(t);
  const requests = reportFetch(t, Buffer.alloc(0), ({ url, options }) => {
    assert.equal(options.body, undefined);
    if (url.pathname === "/reports/royalties")
      return redirect("https://www.amazon.com/ap/signin");
    return html(captchaPage);
  });
  await assert.rejects(
    collect({
      config: {},
      secrets: {
        kdp_username: "fixture@example.invalid",
        kdp_password: "fixture-password-private",
      },
      state: {
        royaltiesByCurrency: [
          { currency: "USD", salesEstimatedRoyalties: 9000 },
        ],
      },
    }),
    { code: "interaction_required" },
  );
  assert.equal(
    requests.some((r) => r.options.method === "POST"),
    false,
  );
  assert.deepEqual(await readdir(directory), []);
});

test("an authenticator prompt ignores alternative SMS links in the selected form", async (t) => {
  await isolated(t);
  const report = await workbook({
    "Combined Sales": [salesHeaders, salesRow()],
  });
  let authenticated = false;
  const requests = reportFetch(t, report, ({ url, options }) => {
    if (url.pathname === "/reports/royalties" && !authenticated)
      return html(
        form(
          [["otpCode"]],
          "https://www.amazon.com/ap/mfa",
          '<p>Enter the code from your authenticator app.</p><a href="/ap/mfa?method=sms">Send a text message instead</a><p hidden>Enter SMS code</p>',
        ),
      );
    if (url.pathname === "/ap/mfa") {
      assert.equal(options.method, "POST");
      assert.match(new URLSearchParams(options.body).get("otpCode"), /^\d{6}$/);
      authenticated = true;
      return redirect("https://kdpreports.amazon.com/reports/royalties");
    }
  });
  const result = await collect({
    config: {},
    secrets: {
      kdp_username: "fixture@example.invalid",
      kdp_password: "fixture-password-private",
      kdp_totp_seed: "JBSWY3DPEHPK3PXP",
    },
  });
  assert.equal(result.data.salesRows, 1);
  assert.equal(requests.filter((r) => r.url.pathname === "/ap/mfa").length, 1);
});

test("missing seed, SMS, approval and unknown MFA methods require interaction", async (t) => {
  await isolated(t);
  let confirmation = form(
    [["otpCode"]],
    "https://www.amazon.com/ap/mfa",
    "Authenticator app",
  );
  const requests = reportFetch(t, Buffer.alloc(0), () => html(confirmation));
  await assert.rejects(
    collect({
      config: {},
      secrets: {
        kdp_username: "fixture@example.invalid",
        kdp_password: "fixture-password-private",
      },
    }),
    { code: "interaction_required" },
  );
  for (const [fields, prompt] of [
    [[["otpCode"]], "Enter SMS text message code"],
    [[["mfaDeviceType", "SMS"], ["otpCode"]], "Enter your code"],
    [
      [["mfaDeviceType", "APPROVAL"], ["otpCode"]],
      "Approve the sign-in request",
    ],
    [[["otpCode"]], "Approve the sign-in request in your authenticator app"],
    [[["mfaDeviceType", "UNKNOWN"], ["otpCode"]], "Authenticator app"],
    [[["otpCode"]], "Enter your code"],
  ]) {
    confirmation = form(
      fields,
      "https://www.amazon.com/ap/mfa",
      prompt +
        '<a href="/ap/mfa?method=totp">Use an authenticator app instead</a>',
    );
    await assert.rejects(
      collect({
        config: {},
        secrets: {
          kdp_username: "fixture@example.invalid",
          kdp_password: "fixture-password-private",
          kdp_totp_seed: "JBSWY3DPEHPK3PXP",
        },
      }),
      { code: "interaction_required" },
      prompt,
    );
  }
  assert.equal(
    requests.some((r) => r.options.method === "POST"),
    false,
  );
});

test("credential rejection and unexpected form actions never submit passwords", async (t) => {
  await isolated(t);
  let invalid = true;
  const requests = reportFetch(t, Buffer.alloc(0), () =>
    html(
      invalid
        ? '<div id="auth-error-message-box"><span>Incorrect credentials</span></div>'
        : form([["password"]], "https://other.example/ap/signin"),
    ),
  );
  const context = {
    config: {},
    secrets: {
      kdp_username: "fixture@example.invalid",
      kdp_password: "fixture-password-private",
    },
  };
  await assert.rejects(collect(context), { code: "authentication_required" });
  invalid = false;
  await assert.rejects(collect(context), { code: "interaction_required" });
  assert.equal(
    requests.some((r) => r.options.method === "POST"),
    false,
  );
});

test("expired API sessions and non-XLSX reports fail without publishing success", async (t) => {
  const directory = await isolated(t);
  let expired = true;
  reportFetch(t, Buffer.from("not a report"), ({ url }) => {
    if (expired && url.pathname === "/api/v2/reports/booksMetadata")
      return redirect("https://www.amazon.com/ap/signin");
    if (url.pathname === "/ap/signin") return html(form([["password"]]));
  });
  const context = {
    config: {},
    secrets: { kdp_cookie: "session=fixture-assigned" },
  };
  await assert.rejects(collect(context), { code: "authentication_required" });
  expired = false;
  await assert.rejects(collect(context), { code: "provider_unavailable" });
  const saved = JSON.parse(
    await readFile(join(directory, ".auth/kdp.json"), "utf8"),
  );
  assert.equal(
    saved.jar.cookies.some((c) => c.value === "fixture-assigned"),
    true,
  );
});

test("an established refreshed session survives a later failed report request", async (t) => {
  const directory = await isolated(t);
  const report = await workbook({
    "Combined Sales": [salesHeaders, salesRow()],
  });
  let failed = true;
  reportFetch(t, report, ({ url, headers }) => {
    if (url.pathname === "/reports/royalties") {
      assert.equal(
        headers.get("Cookie"),
        failed ? "session=fixture-assigned" : "session=fixture-auth-refreshed",
      );
      return new Response(authenticatedPage, {
        headers: {
          "content-type": "text/html",
          "set-cookie":
            "session=fixture-auth-refreshed; Domain=.amazon.com; Path=/; Secure; HttpOnly",
        },
      });
    }
    if (failed && url.pathname.startsWith("/download/report/"))
      return new Response("fixture report error", { status: 503 });
  });
  const context = {
    config: {},
    secrets: { kdp_cookie: "session=fixture-assigned" },
  };
  await assert.rejects(collect(context), { code: "provider_unavailable" });
  const saved = JSON.parse(
    await readFile(join(directory, ".auth/kdp.json"), "utf8"),
  );
  assert.equal(
    saved.jar.cookies.some((c) => c.value === "fixture-auth-refreshed"),
    true,
  );
  failed = false;
  const result = await collect(context);
  assert.equal(
    result.data.royaltiesByCurrency[0].salesEstimatedRoyalties,
    3.75,
  );
});

test("missing configuration, forbidden download hosts and provider failures are actionable", async (t) => {
  await isolated(t);
  let mode = "server";
  const requests = reportFetch(t, Buffer.alloc(0), ({ url }) => {
    if (mode === "server")
      return new Response("fixture server detail", { status: 503 });
    if (url.pathname.startsWith("/download/report/"))
      return json({ url: "https://other.example/report.xlsx" });
  });
  await assert.rejects(collect({ config: {}, secrets: {} }), {
    code: "configuration_invalid",
  });
  assert.equal(requests.length, 0);
  const context = {
    config: {},
    secrets: { kdp_cookie: "session=fixture-assigned" },
  };
  await assert.rejects(collect(context), { code: "provider_unavailable" });
  mode = "download";
  await assert.rejects(collect(context), { code: "provider_unavailable" });
  assert.equal(
    requests.some((r) => r.url.hostname === "other.example"),
    false,
  );
});

test("template stays disabled with explicit secret assignments and pinned task dependencies", async () => {
  const template = JSON.parse(
    await readFile(
      new URL(
        "../docs/examples/data-collector/kdp/royalties.task.json",
        import.meta.url,
      ),
      "utf8",
    ),
  );
  assert.equal(template.enabled, false);
  assert.equal(template.script, "@file:royalties.mjs");
  assert.deepEqual(template.dependencies, {
    parse5: "8.0.1",
    "tough-cookie": "6.0.2",
    exceljs: "4.4.0",
  });
  assert.deepEqual(template.secretNames, ["kdp_username", "kdp_password"]);
  assert.deepEqual(template.retention, {
    maximumCount: 1000,
    maximumAgeDays: 7,
    maximumBytes: 104857600,
  });
});
