import test from "node:test";
import { pathToFileURL } from "node:url";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  rmSync,
  existsSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { spawnSync } from "node:child_process";
import { inventory, selectTests } from "../tools/testing/test-selection.mjs";
import * as config from "../tools/testing/test-config.mjs";

function temporary(t) {
  const root = mkdtempSync(join(tmpdir(), "ivy-runner-unit-"));
  t.after(() => rmSync(root, { recursive: true, force: true, maxRetries: 3 }));
  return root;
}

test("inventory keeps web automation and acceptance tests in their canonical roots", () => {
  const tests = inventory(resolve("."), config);
  const web = tests.filter((item) => item.file.startsWith("tests/web-e2e/"));
  const acceptance = tests.filter((item) =>
    item.file.startsWith("tests/acceptance/tests/"),
  );
  assert.ok(web.length > 0);
  assert.ok(web.every((item) => item.kind === "web"));
  assert.ok(acceptance.length > 0);
  assert.ok(tests.every((item) => item.file.startsWith("tests/")));
  assert.equal(new Set(tests.map((item) => item.file)).size, tests.length);
});

test(
  "test selection keeps Phone changes out of TaskBoard and expands shared Hive/SDK changes",
  { skip: config.privateRepo },
  () => {
    const phone = selectTests(resolve("."), config, {
      changed: ["services/phone-bridge/src/runtime/flow.ts"],
    });
    assert.ok(phone.some((t) => t.file === "tests/phone-flow.test.ts"));
    assert.ok(!phone.some((t) => t.scope === "task-board"));
    for (const changed of [
      "services/hive/src/kernel.ts",
      "packages/sdk/src/client.ts",
    ]) {
      const broad = selectTests(resolve("."), config, { changed: [changed] });
      assert.ok(broad.some((t) => t.scope === "task-board"));
      assert.ok(broad.some((t) => t.scope === "chat"));
    }
  },
);

test(
  "current component paths select their owners without unrelated services",
  { skip: config.privateRepo },
  () => {
    const cases = [
      [
        "services/phone-bridge/src/runtime/flow.ts",
        "phone",
        ["chat", "task-board", "agent"],
      ],
      [
        "services/chat-bridge/src/main.ts",
        "chat",
        ["phone", "task-board", "agent"],
      ],
      [
        "services/task-board/src/runtime/engine.ts",
        "task-board",
        ["phone", "chat"],
      ],
      ["services/agent-manager/src/main.ts", "agent", ["phone"]],
    ];
    for (const [changed, expectedScope, unrelatedScopes] of cases) {
      const selected = selectTests(resolve("."), config, {
        changed: [changed],
      });
      assert.ok(
        selected.some((item) => item.scope === expectedScope),
        changed,
      );
      assert.ok(
        selected.every((item) => !unrelatedScopes.includes(item.scope)),
        changed,
      );
    }
    const native = selectTests(resolve("."), config, {
      changed: ["packages/host-runtime/native/JobLauncher.cs"],
    });
    assert.ok(native.some((item) => item.file === "tests/windows-job.test.ts"));
  },
);

test(
  "automation example inputs select automation checks independently",
  { skip: config.privateRepo },
  () => {
    for (const changed of [
      "docs/examples/services/automation-example/src/main.ts",
      "docs/examples/services/automation-example/src/engine.ts",
      "docs/examples/services/automation-example/deploy.json",
    ]) {
      const selected = selectTests(resolve("."), config, {
        changed: [changed],
      });
      assert.ok(
        selected.some(
          (item) => item.file === "tests/automation-example.test.ts",
        ),
        changed,
      );
      assert.ok(
        selected.every((item) => item.scope === "automation"),
        changed,
      );
    }
  },
);

test("selection follows cyclic fixture imports, deduplicates files and rejects misspelled selectors", (t) => {
  const root = temporary(t);
  mkdirSync(join(root, "tests"));
  mkdirSync(join(root, "src"));
  writeFileSync(join(root, "tests/one.test.mjs"), "import '../src/a.js';");
  writeFileSync(join(root, "tests/two.test.mjs"), "import '../src/c.js';");
  writeFileSync(join(root, "src/a.ts"), "import './b.js';");
  writeFileSync(join(root, "src/b.ts"), "import './a.js'; import './c.js';");
  writeFileSync(join(root, "src/c.ts"), "export const value = 1;");
  const policy = {
    scopes: ["unit"],
    scope: () => "unit",
    kind: () => "core",
    affected: () => [],
  };
  assert.equal(selectTests(root, policy, { changed: ["src/c.ts"] }).length, 2);
  assert.equal(
    selectTests(root, policy, { files: ["tests/*.test.mjs"] }).length,
    2,
  );
  assert.equal(
    selectTests(root, policy, {
      scopes: ["unit"],
      files: ["tests/one.test.mjs", "tests/one.test.mjs"],
    }).length,
    2,
  );
  assert.throws(
    () => selectTests(root, policy, { scopes: ["typo"] }),
    /Unknown test scope/,
  );
  assert.throws(
    () => selectTests(root, policy, { files: ["tests/typo.test.mjs"] }),
    /Unknown test file/,
  );
});

test("partial Rust checkouts select registered files under their owning scopes", (t) => {
  const root = temporary(t);
  mkdirSync(join(root, "tests/rust"), { recursive: true });
  for (const name of ["phone_media.rs", "hive_worker_client.rs", "service-manager-stop.rs"])
    writeFileSync(join(root, "tests/rust", name), "// isolated selector fixture\n");
  const selected = selectTests(root, config, { files: ["tests/rust/*.rs"] });
  assert.deepEqual(selected.map(({ file, scope }) => [file, scope]), [
    ["tests/rust/hive_worker_client.rs", "hive"],
    ["tests/rust/phone_media.rs", "phone"],
    ["tests/rust/service-manager-stop.rs", "host"],
  ]);
  assert.throws(() => selectTests(root, config, { files: ["tests/rust/chat-journal.rs"] }), /Unknown test file/);
});

test("Cargo dispatch groups selected targets and stops after a failed prerequisite or test", (t) => {
  const root = temporary(t), log = join(root, "cargo.jsonl");
  // Node stands in for Cargo, so this exercises subprocess arguments without a build.
  const stub = `const fs=require('node:fs'),path=require('node:path');
    const args=[path.basename(process.argv[1]),...process.argv.slice(2)];
    fs.appendFileSync(process.env.IVY_CARGO_TEST_LOG,JSON.stringify(args)+'\\n');
    if(args[0]===process.env.IVY_CARGO_TEST_FAIL)process.exit(7);`;
  for (const command of ["build", "test"]) writeFileSync(join(root, command), stub);
  const run = (fail = "", files = [
    "tests/rust/agent-interactions.rs", "tests/rust/service-manager-stop.rs",
    "tests/rust/service-manager-policy.rs", "tests/rust/service-manager-stop.rs", "tests/rust/phone_media.rs",
  ]) => {
    writeFileSync(log, "");
    const result = spawnSync(process.execPath, [resolve("tools/testing/test-rust.mjs"), ...files], {
      cwd: root, windowsHide: true, encoding: "utf8", timeout: 15000,
      env: { ...process.env, IVY_CARGO: process.execPath, IVY_CARGO_TEST_LOG: log, IVY_CARGO_TEST_FAIL: fail },
    });
    return { result, commands: readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map(JSON.parse) };
  };
  const expected = [
    ...(process.platform === "win32" ? [["build", "--locked", "--package", "ivy-host-runtime", "--bin", "ivy-job"]] : []),
    ["test", "--locked", "--package", "ivy-agent-manager", "--test", "agent-interactions"],
    ["test", "--locked", "--package", "ivy-service-manager", "--test", "service-manager-stop", "--test", "service-manager-policy"],
    ["test", "--locked", "--package", "ivy-phone-bridge", "--test", "phone_media"],
  ];
  const success = run();
  assert.equal(success.result.status, 0, success.result.stderr);
  assert.deepEqual(success.commands, expected);
  const agentOnly = run("", ["tests/rust/agent-interactions.rs"]);
  assert.equal(agentOnly.result.status, 0, agentOnly.result.stderr);
  assert.deepEqual(agentOnly.commands, expected.slice(0, process.platform === "win32" ? 2 : 1));
  const failure = run(expected[0][0]);
  assert.equal(failure.result.status, 7, failure.result.stderr);
  assert.deepEqual(failure.commands, expected.slice(0, 1));
});

test("reporter suppresses successful output and prints useful failures without writing reports", (t) => {
  const root = temporary(t),
    file = join(root, "sample.test.mjs"),
    reporter = pathToFileURL(resolve("tools/testing/test-reporter.mjs")).href;
  const run = (extra) =>
    spawnSync(
      process.execPath,
      ["--test", "--test-reporter", reporter, ...extra, file],
      {
        encoding: "utf8",
        windowsHide: true,
        env: {
          ...process.env,
          NODE_TEST_CONTEXT: undefined,
          IVY_TEST_PROGRESS: "0",
        },
        timeout: 15000,
      },
    );
  writeFileSync(
    file,
    "import test from 'node:test'; test('success', t => { console.log('NOISY SUCCESS'); t.diagnostic('NOISY DIAGNOSTIC'); });",
  );
  const success = run([]);
  assert.equal(success.status, 0, success.stderr);
  assert.equal(success.stdout, "");
  assert.equal(success.stderr, "");
  writeFileSync(
    file,
    "import test from 'node:test'; import assert from 'node:assert/strict'; test('broken expectation', t => { t.diagnostic('failure diagnostic'); console.error('failure context'); assert.equal(1, 2); });",
  );
  const failure = run([]);
  assert.notEqual(failure.status, 0);
  assert.match(failure.stdout, /FAIL .*broken expectation/);
  assert.match(failure.stdout, /failure context/);
  assert.match(failure.stdout, /failure diagnostic/);
  const empty = run(["--test-name-pattern", "^does-not-exist$"]);
  assert.notEqual(empty.status, 0);
  assert.match(empty.stdout, /no tests executed/);
});

test("runner cleans its temporary workspace after both successful and failed subprocesses", (t) => {
  const root = temporary(t),
    result = join(root, "probe.json");
  for (const fail of ["0", "1"]) {
    const run = spawnSync(
      process.execPath,
      [
        "tools/testing/test.mjs",
        "--files",
        "tests/test-runner.test.mjs",
        "--no-build",
        "--test-name-pattern",
        "^runner probe$",
      ],
      {
        cwd: resolve("."),
        encoding: "utf8",
        windowsHide: true,
        timeout: 15000,
        env: {
          ...process.env,
          IVY_TEST_PROBE_RESULT: result,
          IVY_TEST_PROBE_FAIL: fail,
        },
      },
    );
    assert.equal(run.status, Number(fail), run.stdout + run.stderr);
    const temp = JSON.parse(readFileSync(result, "utf8")).temp;
    assert.equal(
      existsSync(temp),
      false,
      "No success or failure workspace remains after reporting.",
    );
    if (fail === "0") assert.equal(run.stdout + run.stderr, "");
  }
});

test("runner probe", { skip: !process.env.IVY_TEST_PROBE_RESULT }, () => {
  writeFileSync(
    process.env.IVY_TEST_PROBE_RESULT,
    JSON.stringify({ temp: process.env.IVY_TEST_TEMP }),
  );
  writeFileSync(
    join(tmpdir(), "temporary-output.txt"),
    "Only exists during the test.",
  );
  assert.notEqual(process.env.IVY_TEST_PROBE_FAIL, "1", "probe failure");
});

test("runner reuses only successful eligible tests with identical inputs and environment", (t) => {
  const cache = resolve("dist/.test-cache/passed.json");
  t.after(() => rmSync(cache, { force: true }));
  rmSync(cache, { force: true });
  const run = (marker) =>
    spawnSync(
      process.execPath,
      [
        "tools/testing/test.mjs",
        "--files",
        "tests/build-cache.test.mjs",
        "--reuse",
        "--progress",
      ],
      {
        cwd: resolve("."),
        encoding: "utf8",
        windowsHide: true,
        timeout: 30000,
        env: { ...process.env, IVY_CACHE_TEST_MARKER: marker },
      },
    );
  const first = run("one");
  assert.equal(first.status, 0, first.stdout + first.stderr);
  assert.doesNotMatch(first.stdout, /Reusing/);
  const second = run("one");
  assert.equal(second.status, 0, second.stdout + second.stderr);
  assert.match(second.stdout, /Reusing/);
  const changed = run("two");
  assert.equal(changed.status, 0, changed.stdout + changed.stderr);
  assert.doesNotMatch(changed.stdout, /Reusing/);
});

test("implicit changes cannot launch a broad campaign and timeout arguments fail before execution", async () => {
  const { implicitSelectionTooLarge } =
    await import("../tools/testing/test-selection.mjs");
  assert.equal(implicitSelectionTooLarge(20, false), false);
  assert.equal(implicitSelectionTooLarge(21, false), true);
  assert.equal(implicitSelectionTooLarge(200, true), false);
  const run = spawnSync(
    process.execPath,
    [
      "tools/testing/test.mjs",
      "--files",
      "tests/build-cache.test.mjs",
      "--timeout-ms",
      "0",
    ],
    {
      cwd: resolve("."),
      encoding: "utf8",
      windowsHide: true,
      timeout: 10000,
    },
  );
  assert.notEqual(run.status, 0);
  assert.match(run.stderr, /--timeout-ms must be/);
});
