import test from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";
import collectBlink, {
  extractLastClipFrame,
} from "../docs/examples/data-collector/cameras/blink.mjs";
import collectRtsp, {
  captureFrame,
} from "../docs/examples/data-collector/cameras/rtsp.mjs";

const jpeg = Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0x01, 0xff, 0xd9]);
const json = (data) =>
  new Response(JSON.stringify(data), {
    headers: { "content-type": "application/json" },
  });
const camera = {
  id: "123",
  networkId: "456",
  type: "camera",
  name: "Entrance",
};
const blinkConfig = {
  accountId: "789",
  region: "u001",
  cameras: [camera],
  freshSnapshots: false,
  motion: false,
};
const homescreen = (thumbnail = "1700000000") => ({
  cameras: [
    { id: 123, network_id: 456, thumbnail, type: "catalina", name: "Entrance" },
  ],
});
const blinkSecrets = { BLINK_ACCESS_TOKEN: "fixture-token" };

function blinkFixture({ home = homescreen(), media = [], fail } = {}) {
  return async (url, options) => {
    assert.equal(new URL(url).origin, "https://rest-u001.immedia-semi.com");
    assert.equal(options.headers.Authorization, "Bearer fixture-token");
    assert.equal(options.redirect, "error");
    if (fail) return fail(url, options);
    if (url.includes("/homescreen")) return json(home);
    if (url.includes("/media/changed")) return json({ media });
    return new Response(jpeg, { headers: { "content-type": "image/jpeg" } });
  };
}

test("Blink fresh snapshots use real device routes and wait for a changed thumbnail", async () => {
  for (const [type, group, deviceType, route] of [
    ["camera", "cameras", "catalina", "/network/456/camera/123/thumbnail"],
    [
      "mini",
      "owls",
      "owl",
      "/api/v1/accounts/789/networks/456/owls/123/thumbnail",
    ],
    [
      "doorbell",
      "doorbells",
      "lotus",
      "/api/v1/accounts/789/networks/456/doorbells/123/thumbnail",
    ],
  ]) {
    let homescreenCalls = 0;
    const calls = [];
    const result = await collectBlink(
      {
        config: {
          ...blinkConfig,
          freshSnapshots: true,
          cameras: [{ ...camera, type }],
        },
        secrets: blinkSecrets,
      },
      async (url, options) => {
        calls.push({ path: new URL(url).pathname, method: options.method });
        if (url.includes("/homescreen"))
          return json({
            [group]: [
              {
                id: 123,
                network_id: 456,
                type: deviceType,
                thumbnail:
                  ++homescreenCalls === 1 ? "1700000000" : "1700000300",
              },
            ],
          });
        if (options.method === "POST")
          return json({ id: 987, network_id: 456 });
        if (url.includes("/command/987"))
          return json({ complete: true, status_code: 908 });
        assert.ok(
          url.includes(
            `/${deviceType}/123/thumbnail/thumbnail.jpg?ts=1700000300&ext=`,
          ),
        );
        return new Response(jpeg);
      },
    );
    assert.deepEqual(
      calls.find((call) => call.method === "POST"),
      { path: route, method: "POST" },
    );
    assert.equal(result.data.images[0].capturedAt, "2023-11-14T22:18:20.000Z");
    assert.equal(
      result.data.images[0].dataUrl,
      `data:image/jpeg;base64,${jpeg.toString("base64")}`,
    );
    assert.equal(result.data.cameras[0].fresh, true);
  }
});

test("Blink motion baselines then deduplicates PIR media, excluding manual and deleted clips", async () => {
  const clip = (id, extra = {}) => ({
    id,
    device_id: 123,
    network_id: 456,
    type: "video",
    source: "pir",
    created_at: new Date().toISOString(),
    ...extra,
  });
  const config = { ...blinkConfig, motion: true };
  const first = await collectBlink(
    { config, secrets: blinkSecrets },
    blinkFixture({ media: [clip(1)] }),
  );
  assert.deepEqual(first.events, []);
  assert.equal(first.data.motion.baseline, true);
  const media = [
    clip(1),
    clip(2),
    clip(3, { source: "manual" }),
    clip(4, { deleted: true }),
    clip(5, { device_id: 999 }),
  ];
  const second = await collectBlink(
    { config, secrets: blinkSecrets, state: first.state },
    blinkFixture({ media }),
  );
  assert.equal(second.events.length, 1);
  assert.deepEqual(second.events[0], {
    name: "motion",
    payload: {
      cameraId: "123",
      name: "Entrance",
      mediaId: "2",
      detectedAt: media[1].created_at,
      source: "blink-cloud",
    },
  });
  const third = await collectBlink(
    { config, secrets: blinkSecrets, state: second.state },
    blinkFixture({ media }),
  );
  assert.deepEqual(third.events, []);
  assert.ok(!JSON.stringify(third).includes("fixture-token"));
});

test("Blink unavailable media leaves images usable and preserves the motion cursor", async () => {
  const state = {
    motion: {
      checkedAt: new Date().toISOString(),
      seenIds: ["456:camera:123:1"],
    },
  };
  const request = blinkFixture({
    fail: (url) =>
      url.includes("/media/changed")
        ? new Response("unsupported", { status: 404 })
        : url.includes("/homescreen")
          ? json(homescreen())
          : new Response(jpeg),
  });
  const result = await collectBlink(
    { config: { ...blinkConfig, motion: true }, secrets: blinkSecrets, state },
    request,
  );
  assert.equal(result.data.images.length, 1);
  assert.equal(result.data.motion.available, false);
  assert.match(result.data.motion.reason, /HTTP 404/);
  assert.deepEqual(result.state, state);
});

test("Blink local clips baseline, deduplicate and trigger snapshots without waking idle cameras", async () => {
  const providerName = "Front Door";
  const config = {
    ...blinkConfig,
    localStorage: true,
    freshSnapshots: true,
    snapshotIntervalSeconds: 300,
    cameras: [{ ...camera, name: "Display label" }],
  };
  const clip = (id) => ({
    id: String(id),
    camera_name: "FrontDoor",
    created_at: new Date().toISOString(),
  });
  let clips = [clip(1)],
    thumbnailTime = 1700000000,
    snapshots = 0;
  let rejectSnapshot = false,
    rejectManifest = false;
  const request = async (url, options) => {
    const path = new URL(url).pathname;
    if (path.endsWith("/homescreen"))
      return json({
        cameras: [
          {
            ...homescreen().cameras[0],
            name: providerName,
            thumbnail: `/thumbnail.jpg?ts=${thumbnailTime}&ext=`,
          },
        ],
        sync_modules: [
          {
            id: 654,
            network_id: 456,
            local_storage_enabled: true,
            local_storage_status: "active",
          },
        ],
      });
    if (path.endsWith("/manifest/request")) {
      if (rejectManifest) return new Response("busy", { status: 503 });
      return json({ id: 900 });
    }
    if (path.endsWith("/manifest/request/900"))
      return json({ manifest_id: "99", clips });
    if (options.method === "POST" && path.endsWith("/thumbnail")) {
      snapshots++;
      if (rejectSnapshot) return new Response("busy", { status: 503 });
      thumbnailTime += 300;
      return json({ id: 901 });
    }
    if (path.includes("/command/"))
      return json({ status_code: 908, complete: true });
    assert.equal(path, "/thumbnail.jpg");
    return new Response(jpeg);
  };
  const collect = (state) =>
    collectBlink({ config, secrets: blinkSecrets, state }, request);
  const first = await collect();
  assert.equal(first.data.localStorage.baseline, true);
  assert.deepEqual(first.events, []);
  assert.equal(snapshots, 1);
  const second = await collect(first.state);
  assert.equal(snapshots, 1);
  assert.equal(second.data.cameras[0].fresh, false);
  assert.equal(
    second.data.images[0].capturedAt,
    first.data.images[0].capturedAt,
  );
  clips.push(clip(2));
  const third = await collect(second.state);
  assert.equal(snapshots, 2);
  assert.equal(third.data.cameras[0].fresh, true);
  assert.equal(third.events.length, 1);
  assert.deepEqual(third.events[0], {
    name: "recording",
    payload: {
      cameraId: "123",
      name: "Display label",
      clipId: "2",
      recordedAt: clips[1].created_at,
      source: "blink-local-storage",
    },
  });
  const fourth = await collect(third.state);
  assert.equal(snapshots, 2);
  assert.deepEqual(fourth.events, []);
  clips.push(clip(3));
  rejectSnapshot = true;
  // Another usable camera image allows publication of the failed camera's
  // pending trigger; the next poll must retry even though the clip is now seen.
  const retryConfig = {
    ...config,
    cameras: [
      ...config.cameras,
      { id: "999", networkId: "456", type: "camera", name: "Other" },
    ],
  };
  const retryRequest = async (url, options) => {
    if (url.includes("/homescreen")) {
      const response = await request(url, options),
        home = await response.json();
      home.cameras.push({
        id: 999,
        network_id: 456,
        name: "Other",
        thumbnail: "/thumbnail.jpg?ts=1700000000&ext=",
      });
      return json(home);
    }
    return request(url, options);
  };
  const retryState = {
    ...fourth.state,
    snapshots: {
      ...fourth.state.snapshots,
      "456:camera:999": {
        updatedAt: new Date().toISOString(),
        thumbnail: "/thumbnail.jpg?ts=1700000000&ext=",
        capturedAt: "2023-11-14T22:13:20.000Z",
      },
    },
  };
  const failed = await collectBlink(
    { config: retryConfig, secrets: blinkSecrets, state: retryState },
    retryRequest,
  );
  assert.equal(failed.data.cameras[0].status, "error");
  assert.equal(failed.state.snapshots["456:camera:123"].pending, true);
  rejectSnapshot = false;
  const retried = await collect(failed.state);
  assert.equal(retried.data.cameras[0].fresh, true);
  assert.deepEqual(retried.events, []);
  rejectManifest = true;
  const unavailable = await collect(retried.state);
  assert.equal(unavailable.data.localStorage.available, false);
  assert.deepEqual(unavailable.state.localStorage, retried.state.localStorage);
  assert.deepEqual(unavailable.events, []);
  assert.equal(unavailable.data.cameras[0].fresh, false);
  const expired = await collect({
    ...retried.state,
    snapshots: {
      "456:camera:123": {
        ...retried.state.snapshots["456:camera:123"],
        updatedAt: new Date(Date.now() - 301000).toISOString(),
      },
    },
  });
  assert.equal(expired.data.cameras[0].fresh, true);
});

test("Blink newer local clip frames are cached, retried after failure and never request camera captures", async (t) => {
  const workspace = await authWorkspace(t);
  const now = Math.floor(Date.now() / 1000) * 1000 - 30000;
  const config = { ...blinkConfig, localStorage: true, localClipImages: true };
  const clip = (id, seconds) => ({
    id: String(id),
    camera_name: "Entrance",
    created_at: new Date(now + seconds * 1000).toISOString(),
  });
  const video = Buffer.concat([
    Buffer.from([0, 0, 0, 12]),
    Buffer.from("ftypfixture"),
  ]);
  const clipJpeg = Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0x02, 0xff, 0xd9]);
  let clips = [clip(1, -60), clip(9, -180)];
  let thumbnail = String((now - 120000) / 1000);
  let downloads = 0,
    extractions = 0,
    thumbnails = 0,
    failClip = false,
    failManifest = false;
  let offsetSeconds = 5;
  const calls = [];
  const request = async (url, options) => {
    const path = new URL(url).pathname;
    calls.push({ path, method: options.method });
    if (path.endsWith("/homescreen"))
      return json({
        ...homescreen(thumbnail),
        sync_modules: [
          {
            id: 654,
            network_id: 456,
            local_storage_enabled: true,
            local_storage_status: "active",
          },
        ],
      });
    if (path.endsWith("/manifest/request"))
      return failManifest
        ? new Response(null, { status: 503 })
        : json({ id: 900 });
    if (path.endsWith("/manifest/request/900"))
      return json({ manifest_id: "99", clips });
    if (path.includes("/command/"))
      return json({ complete: true, status_code: 908 });
    if (path.includes("/clip/request/")) {
      assert.ok(path.endsWith(clips[0].id));
      if (options.method === "POST") return json({ id: 901 });
      downloads++;
      return failClip
        ? new Response(null, { status: 503 })
        : new Response(video);
    }
    assert.equal(options.method, "GET");
    assert.ok(path.endsWith("/thumbnail/thumbnail.jpg"));
    thumbnails++;
    return new Response(jpeg);
  };
  const extract = async (options) => {
    extractions++;
    assert.deepEqual(await readFile(options.inputPath), video);
    assert.equal(options.width, 960);
    return { bytes: clipJpeg, offsetSeconds };
  };
  const collect = (state) =>
    collectBlink(
      { config, secrets: blinkSecrets, state },
      request,
      workspace.directory,
      extract,
    );
  const first = await collect();
  assert.deepEqual(first.events, []);
  assert.equal(first.data.cameras[0].fresh, false);
  assert.equal(first.data.images[0].source, "blink-local-clip");
  assert.equal(
    first.data.images[0].capturedAt,
    new Date(now - 55000).toISOString(),
  );
  assert.equal(
    first.data.images[0].dataUrl,
    `data:image/jpeg;base64,${clipJpeg.toString("base64")}`,
  );
  const idle = await collect(first.state);
  assert.deepEqual(idle.data.images, first.data.images);
  assert.equal(downloads, 1);
  assert.equal(extractions, 1);
  assert.equal(thumbnails, 0);

  // A newer existing thumbnail wins over the cached clip, without downloading
  // the old video again. Retrieval time must not make an old image appear new.
  thumbnail = String((now - 30000) / 1000);
  const newerThumbnail = await collect(idle.state);
  assert.equal(newerThumbnail.data.images[0].source, "blink-thumbnail");
  assert.equal(
    newerThumbnail.data.images[0].capturedAt,
    new Date(now - 30000).toISOString(),
  );
  assert.equal(downloads, 1);

  clips = [clip(2, -10), ...clips];
  failClip = true;
  const failed = await collect(newerThumbnail.state);
  assert.equal(failed.events.length, 1);
  assert.deepEqual(failed.data.images, newerThumbnail.data.images);
  assert.match(failed.data.cameras[0].clipError, /HTTP 503/);
  failClip = false;
  const retried = await collect(failed.state);
  assert.deepEqual(retried.events, []);
  assert.equal(retried.data.images[0].clipId, "2");
  assert.equal(
    retried.data.images[0].capturedAt,
    new Date(now - 5000).toISOString(),
  );
  assert.equal(downloads, 3);
  assert.equal(extractions, 2);

  thumbnail = String((now - 2000) / 1000);
  const duringClip = await collect(retried.state);
  clips = [clip(3, -4), ...clips];
  offsetSeconds = 3;
  const laterLastFrame = await collect(duringClip.state);
  assert.equal(laterLastFrame.data.images[0].clipId, "3");
  assert.equal(
    laterLastFrame.data.images[0].capturedAt,
    new Date(now - 1000).toISOString(),
  );
  assert.equal(downloads, 4);

  // The newest stored clip is downloaded once to determine its last frame time,
  // but it must never replace an image that was captured later.
  thumbnail = String((now + 10000) / 1000);
  clips = [clip(4, 5), ...clips];
  const laterThumbnail = await collect(laterLastFrame.state);
  assert.equal(laterThumbnail.data.images[0].source, "blink-thumbnail");
  assert.equal(
    laterThumbnail.data.images[0].capturedAt,
    new Date(now + 10000).toISOString(),
  );
  assert.equal(downloads, 5);
  const olderClipIdle = await collect(laterThumbnail.state);
  assert.deepEqual(olderClipIdle.data.images, laterThumbnail.data.images);
  assert.equal(downloads, 5);

  failManifest = true;
  const unavailable = await collect(olderClipIdle.state);
  assert.equal(unavailable.data.localStorage.available, false);
  assert.deepEqual(unavailable.data.images, laterThumbnail.data.images);
  assert.equal(downloads, 5);
  assert.ok(
    calls
      .filter((call) => call.method === "POST")
      .every((call) => call.path.includes("/local_storage/")),
  );
  assert.ok(calls.every((call) => !call.path.includes("/liveview")));
});

test("Blink saved clip mode rejects camera wakes and bounds clip downloads", async (t) => {
  const workspace = await authWorkspace(t);
  const config = {
    ...blinkConfig,
    localStorage: true,
    localClipImages: true,
    maximumClipBytes: 1024,
  };
  await assert.rejects(
    collectBlink(
      { config: { ...config, freshSnapshots: true }, secrets: blinkSecrets },
      () => assert.fail("invalid mode must not access the provider"),
    ),
    /requires localStorage: true and freshSnapshots: false/,
  );
  const result = await collectBlink(
    { config, secrets: blinkSecrets },
    async (url, options) => {
      if (url.includes("/homescreen"))
        return json({
          ...homescreen(),
          sync_modules: [
            {
              id: 654,
              network_id: 456,
              local_storage_enabled: true,
              local_storage_status: "active",
            },
          ],
        });
      if (url.includes("/command/"))
        return json({ complete: true, status_code: 908 });
      if (options.method === "POST") {
        assert.ok(url.includes("/local_storage/"));
        return json({ id: 900 });
      }
      if (url.includes("/manifest/request/900"))
        return json({
          manifest_id: "99",
          clips: [
            {
              id: "1",
              camera_name: "Entrance",
              created_at: new Date(Date.now() - 1000).toISOString(),
            },
          ],
        });
      if (url.includes("/clip/request/"))
        return new Response(Buffer.alloc(1025));
      return new Response(jpeg);
    },
    workspace.directory,
    () => assert.fail("oversized clips must not be decoded"),
  );
  assert.equal(result.data.images[0].source, "blink-thumbnail");
  assert.equal(result.data.cameras[0].fresh, false);
  assert.match(result.data.cameras[0].clipError, /size limit/);
});

test("Blink last-frame extraction restricts FFmpeg to saved files, reads the final JPEG and terminates hangs", async (t) => {
  const workspace = await authWorkspace(t);
  const options = {
    ffmpegPath: "fixture-ffmpeg",
    inputPath: join(workspace.directory, "clip.mp4"),
    outputPath: join(workspace.directory, "last.jpg"),
    width: 960,
    quality: 7,
    maximumImageBytes: 1024,
    timeoutMs: 1000,
  };
  const success = fakeSpawn(async (child) => {
    await writeFile(options.outputPath, jpeg);
    child.stdout.end("out_time_us=3250000\nprogress=end\n");
    child.emit("close", 0);
  });
  const frame = await extractLastClipFrame(
    options,
    undefined,
    success.spawnProcess,
  );
  assert.deepEqual(frame.bytes, jpeg);
  assert.equal(frame.offsetSeconds, 3.25);
  const args = success.child.invocation.args;
  assert.equal(args[args.indexOf("-protocol_whitelist") + 1], "file,pipe");
  assert.equal(args[args.indexOf("-i") + 1], options.inputPath);
  assert.equal(args[args.indexOf("-update") + 1], "1");
  assert.ok(!args.includes("-frames:v"));
  assert.equal(success.child.invocation.options.shell, false);
  const oversized = fakeSpawn(async (child) => {
    await writeFile(options.outputPath, Buffer.alloc(1025));
    child.stdout.end("out_time_us=1000000\nprogress=end\n");
    child.emit("close", 0);
  });
  await assert.rejects(
    extractLastClipFrame(options, undefined, oversized.spawnProcess),
    /exceeds maximumImageBytes/,
  );
  const hung = fakeSpawn();
  await assert.rejects(
    extractLastClipFrame(
      { ...options, timeoutMs: 10 },
      undefined,
      hung.spawnProcess,
    ),
    /timed out/,
  );
  assert.equal(hung.child.killedWith, "SIGKILL");
});

test("Blink ambiguous local camera names preserve the clip cursor without false triggers", async () => {
  const state = {
    localStorage: {
      "456:654": {
        checkedAt: new Date().toISOString(),
        seenIds: [],
      },
    },
  };
  const result = await collectBlink(
    {
      config: {
        ...blinkConfig,
        localStorage: true,
        cameras: [camera, { id: "999", networkId: "456", type: "camera" }],
      },
      secrets: blinkSecrets,
      state,
    },
    async (url, options) => {
      if (url.includes("/homescreen"))
        return json({
          cameras: [
            homescreen().cameras[0],
            {
              id: 999,
              network_id: 456,
              name: "Entrance",
              thumbnail: "1700000000",
            },
          ],
          sync_modules: [
            {
              id: 654,
              network_id: 456,
              local_storage_enabled: true,
              local_storage_status: "active",
            },
          ],
        });
      if (options.method === "POST") return json({ id: 900 });
      if (url.includes("/command/"))
        return json({ complete: true, status_code: 908 });
      if (url.includes("/manifest/request/900"))
        return json({
          manifest_id: "1",
          clips: [
            {
              id: "1",
              camera_name: "Entrance",
              created_at: new Date(Date.now() - 1000).toISOString(),
            },
          ],
        });
      return new Response(jpeg);
    },
  );
  assert.equal(result.data.localStorage.available, false);
  assert.match(result.data.localStorage.reason, /ambiguous/);
  assert.deepEqual(result.state, state);
  assert.deepEqual(result.events, []);
});

test("Blink missing cameras do not prevent other configured cameras from returning images", async () => {
  const result = await collectBlink(
    {
      config: {
        ...blinkConfig,
        cameras: [camera, { id: "999", networkId: "456", type: "camera" }],
      },
      secrets: blinkSecrets,
    },
    blinkFixture(),
  );
  assert.equal(result.data.images.length, 1);
  assert.equal(result.data.cameras[1].status, "error");
  assert.match(result.data.cameras[1].error, /not found/);
});

test("Blink motion page limits preserve the cursor and discard incomplete event batches", async () => {
  const state = {
    motion: { checkedAt: new Date().toISOString(), seenIds: [] },
  };
  const request = blinkFixture({
    fail: (url) =>
      url.includes("/media/changed")
        ? json({
            limit: 1,
            media: [
              {
                id: 2,
                device_id: 123,
                network_id: 456,
                source: "pir",
                type: "video",
                created_at: new Date().toISOString(),
              },
            ],
          })
        : url.includes("/homescreen")
          ? json(homescreen())
          : new Response(jpeg),
  });
  const result = await collectBlink(
    {
      config: { ...blinkConfig, motion: true, maximumMotionPages: 1 },
      secrets: blinkSecrets,
      state,
    },
    request,
  );
  assert.match(result.data.motion.reason, /pagination/);
  assert.deepEqual(result.state, state);
  assert.deepEqual(result.events, []);
});

test("Blink expired authorization and transport failures never echo token or response bodies", async () => {
  await assert.rejects(
    collectBlink(
      { config: blinkConfig, secrets: blinkSecrets },
      async () =>
        new Response("fixture-token private response", { status: 401 }),
    ),
    (error) =>
      /authorization failed/.test(error.message) &&
      !error.message.includes("fixture-token"),
  );
  await assert.rejects(
    collectBlink({ config: blinkConfig, secrets: blinkSecrets }, async () => {
      throw new Error("Blink fixture-token https://private.example/");
    }),
    /^Error: Blink network request failed\.$/,
  );
});

test("Blink refuses cross-origin thumbnails before sending credentials", async () => {
  let calls = 0;
  await assert.rejects(
    collectBlink({ config: blinkConfig, secrets: blinkSecrets }, async () => {
      calls++;
      return json(homescreen("https://other.example/thumbnail.jpg"));
    }),
    /outside the configured API origin/,
  );
  assert.equal(calls, 1);
});

test("Blink rejects busy snapshot commands, invalid JPEGs and oversized images", async () => {
  await assert.rejects(
    collectBlink(
      {
        config: { ...blinkConfig, freshSnapshots: true },
        secrets: blinkSecrets,
      },
      async (url, options) => {
        if (url.includes("/homescreen")) return json(homescreen());
        if (options.method === "POST") return json({ id: 987 });
        return json({ complete: true, status_code: 307 });
      },
    ),
    /camera is busy/,
  );
  await assert.rejects(
    collectBlink(
      { config: blinkConfig, secrets: blinkSecrets },
      blinkFixture({
        fail: (url) =>
          url.includes("/homescreen")
            ? json(homescreen())
            : new Response("not jpeg"),
      }),
    ),
    /complete JPEG/,
  );
  await assert.rejects(
    collectBlink(
      {
        config: { ...blinkConfig, maximumImageBytes: 1024 },
        secrets: blinkSecrets,
      },
      blinkFixture({
        fail: (url) =>
          url.includes("/homescreen")
            ? json(homescreen())
            : new Response(Buffer.alloc(1025)),
      }),
    ),
    /size limit/,
  );
});

test("Blink completed snapshot commands cannot silently return stale thumbnails", async () => {
  let imageCalls = 0;
  await assert.rejects(
    collectBlink(
      {
        config: {
          ...blinkConfig,
          freshSnapshots: true,
          snapshotTimeoutMs: 100,
        },
        secrets: blinkSecrets,
      },
      async (url, options) => {
        if (url.includes("/homescreen")) return json(homescreen());
        if (options.method === "POST") return json({ id: 987 });
        if (url.includes("/command/"))
          return json({ complete: true, status_code: 908 });
        imageCalls++;
        return new Response(jpeg);
      },
    ),
    /no fresh thumbnail/,
  );
  assert.equal(imageCalls, 0);
});

test("Blink HTTP deadlines cover stalled requests", async () => {
  await assert.rejects(
    collectBlink(
      {
        config: { ...blinkConfig, requestTimeoutMs: 100 },
        secrets: blinkSecrets,
      },
      async (_url, { signal }) =>
        await new Promise((_resolve, reject) =>
          signal.addEventListener("abort", () => reject(signal.reason), {
            once: true,
          }),
        ),
    ),
    /timed out/,
  );
});

const oauthOrigin = "https://api.oauth.blink.com";
const hardwareId = "12345678-1234-1234-1234-123456789abc";
const refreshSecrets = {
  BLINK_REFRESH_TOKEN: "fixture-refresh-seed",
  BLINK_HARDWARE_ID: hardwareId,
};
const passwordSecrets = {
  BLINK_EMAIL: "camera-user@example.com",
  BLINK_PASSWORD: "fixture-password",
};
const tokenPair = (suffix = "one") => ({
  access_token: `fixture-access-${suffix}`,
  refresh_token: `fixture-refresh-${suffix}`,
  expires_in: 3600,
});
const errorCode = (code) => (error) => {
  assert.equal(error.code, code);
  for (const secret of [
    "fixture-password",
    "fixture-refresh",
    "fixture-access",
    "camera-user@example.com",
    "fixture-cookie",
    "246810",
    "135790",
  ])
    assert.ok(!error.message.includes(secret), error.message);
  return true;
};
async function authWorkspace(t) {
  const directory = await mkdtemp(join(tmpdir(), "ivy-blink-test-"));
  const parent = resolve(tmpdir()) + sep;
  assert.ok(resolve(directory).startsWith(parent));
  t.after(async () => {
    assert.ok(resolve(directory).startsWith(parent));
    await rm(directory, { recursive: true, force: true });
  });
  return {
    directory,
    path: join(directory, ".auth", "blink.json"),
    async cache() {
      return JSON.parse(await readFile(this.path, "utf8"));
    },
  };
}
function cameraResponse(url) {
  assert.equal(new URL(url).origin, "https://rest-u001.immedia-semi.com");
  return url.includes("/homescreen") ? json(homescreen()) : new Response(jpeg);
}

test("Blink prefers an access token and discovers account and camera identifiers without snapshots", async (t) => {
  const workspace = await authWorkspace(t);
  const calls = [];
  const result = await collectBlink(
    {
      config: { discoveryOnly: true },
      secrets: { ...blinkSecrets, ...passwordSecrets },
    },
    async (url, options) => {
      calls.push(url);
      assert.equal(options.headers.Authorization, "Bearer fixture-token");
      if (url === "https://rest-prod.immedia-semi.com/api/v1/users/tier_info")
        return json({ account_id: 789, tier: "u001" });
      return json({
        ...homescreen(),
        owls: [{ id: 999, network_id: 456, name: "Mini" }],
        doorbells: [{ id: 100, network_id: 457 }],
      });
    },
    workspace.directory,
  );
  assert.deepEqual(result.data, {
    accountId: "789",
    region: "u001",
    cameraInventory: [
      { id: "123", networkId: "456", type: "camera", name: "Entrance" },
      { id: "999", networkId: "456", type: "mini", name: "Mini" },
      { id: "100", networkId: "457", type: "doorbell", name: "100" },
    ],
  });
  assert.equal(calls.length, 2);
  assert.ok(!calls.some((url) => url.startsWith(oauthOrigin)));
  assert.ok(!JSON.stringify(result).includes("fixture-token"));
});

test("Blink rotates refresh tokens before collection and uses that rotation after a failed read", async (t) => {
  const workspace = await authWorkspace(t);
  let tokenCalls = 0;
  const request = async (url, options) => {
    if (url === `${oauthOrigin}/oauth/token`) {
      const fields = new URLSearchParams(options.body);
      assert.equal(fields.get("grant_type"), "refresh_token");
      assert.equal(fields.get("client_id"), "ios");
      assert.equal(fields.get("scope"), "client");
      assert.equal(fields.get("hardware_id"), hardwareId);
      assert.equal(
        fields.get("refresh_token"),
        ++tokenCalls === 1 ? "fixture-refresh-seed" : "fixture-refresh-one",
      );
      return json(tokenPair(tokenCalls === 1 ? "one" : "two"));
    }
    if (options.headers.Authorization === "Bearer fixture-access-one") {
      const cache = await workspace.cache();
      assert.equal(cache.refreshToken, "fixture-refresh-one");
      return new Response("provider response fixture-access-one", {
        status: 503,
      });
    }
    assert.equal(options.headers.Authorization, "Bearer fixture-access-two");
    return cameraResponse(url);
  };
  await assert.rejects(
    collectBlink(
      { config: blinkConfig, secrets: refreshSecrets },
      request,
      workspace.directory,
    ),
    errorCode("provider_unavailable"),
  );
  assert.equal(tokenCalls, 1);
  const cache = await workspace.cache();
  cache.expiresAt = Date.now() - 1000;
  await writeFile(workspace.path, JSON.stringify(cache));
  const result = await collectBlink(
    { config: blinkConfig, secrets: refreshSecrets },
    request,
    workspace.directory,
  );
  assert.equal(tokenCalls, 2);
  assert.equal((await workspace.cache()).refreshToken, "fixture-refresh-two");
  assert.ok(!JSON.stringify(result).includes("fixture-access"));
  await collectBlink(
    { config: blinkConfig, secrets: refreshSecrets },
    request,
    workspace.directory,
  );
  assert.equal(tokenCalls, 2);
  if (process.platform !== "win32") {
    assert.equal((await stat(workspace.path)).mode & 0o777, 0o600);
    assert.equal(
      (await stat(join(workspace.directory, ".auth"))).mode & 0o777,
      0o700,
    );
  }
});

test("Blink retries one rejected access token with refreshed credentials", async (t) => {
  const workspace = await authWorkspace(t);
  let refreshCalls = 0,
    rejected = 0;
  const result = await collectBlink(
    { config: blinkConfig, secrets: { ...blinkSecrets, ...refreshSecrets } },
    async (url, options) => {
      if (url === `${oauthOrigin}/oauth/token`) {
        refreshCalls++;
        return json(tokenPair());
      }
      if (options.headers.Authorization === "Bearer fixture-token") {
        rejected++;
        return new Response(null, { status: 401 });
      }
      assert.equal(options.headers.Authorization, "Bearer fixture-access-one");
      return cameraResponse(url);
    },
    workspace.directory,
  );
  assert.equal(rejected, 1);
  assert.equal(refreshCalls, 1);
  assert.equal(result.data.images.length, 1);
});

test("Blink revoked refresh credentials are not retried by every scheduled run, and replacements reset the cache", async (t) => {
  const workspace = await authWorkspace(t);
  let rejected = 0;
  const revoke = async (url) => {
    assert.equal(url, `${oauthOrigin}/oauth/token`);
    rejected++;
    return new Response("fixture-refresh-seed private", { status: 400 });
  };
  for (let run = 0; run < 2; run++)
    await assert.rejects(
      collectBlink(
        { config: blinkConfig, secrets: refreshSecrets },
        revoke,
        workspace.directory,
      ),
      errorCode("authentication_required"),
    );
  assert.equal(rejected, 1);
  const result = await collectBlink(
    {
      config: blinkConfig,
      secrets: {
        ...refreshSecrets,
        BLINK_REFRESH_TOKEN: "replacement-refresh",
      },
    },
    async (url, options) => {
      if (url === `${oauthOrigin}/oauth/token`) {
        assert.equal(
          new URLSearchParams(options.body).get("refresh_token"),
          "replacement-refresh",
        );
        return json(tokenPair());
      }
      return cameraResponse(url);
    },
    workspace.directory,
  );
  assert.equal(result.data.images.length, 1);
});

test("Blink transient refresh failure does not trigger password signin or leak transport details", async (t) => {
  const workspace = await authWorkspace(t);
  for (const response of [
    new Response("fixture-password fixture-refresh-seed", { status: 429 }),
    null,
  ]) {
    await assert.rejects(
      collectBlink(
        {
          config: blinkConfig,
          secrets: { ...refreshSecrets, ...passwordSecrets },
        },
        async (url) => {
          assert.equal(url, `${oauthOrigin}/oauth/token`);
          if (!response)
            throw new Error(
              "fixture-password fixture-cookie https://private.example/",
            );
          return response;
        },
        workspace.directory,
      ),
      errorCode("provider_unavailable"),
    );
  }
});

function passwordTranscript({
  challenge = 412,
  verifyStatus = 201,
  failCollection = false,
} = {}) {
  const counts = { authorize: 0, signin: 0, verify: 0, exchange: 0 };
  let challengeHash;
  const request = async (url, options) => {
    const parsed = new URL(url);
    if (parsed.origin !== oauthOrigin)
      return failCollection
        ? new Response("private fixture-access-one", { status: 503 })
        : cameraResponse(url);
    assert.equal(options.redirect, "manual");
    const cookie = options.headers.Cookie ?? "";
    if (parsed.pathname === "/oauth/v2/authorize" && parsed.search) {
      counts.authorize++;
      assert.equal(parsed.searchParams.get("client_id"), "ios");
      assert.equal(
        parsed.searchParams.get("redirect_uri"),
        "immedia-blink://applinks.blink.com/signin/callback",
      );
      assert.equal(parsed.searchParams.get("code_challenge_method"), "S256");
      assert.match(parsed.searchParams.get("hardware_id"), /^[0-9a-f-]{36}$/i);
      challengeHash = parsed.searchParams.get("code_challenge");
      return new Response("authorized", {
        headers: {
          "set-cookie": "session=fixture-cookie; Secure; HttpOnly; Path=/oauth",
        },
      });
    }
    assert.match(cookie, /session=fixture-cookie/);
    if (parsed.pathname !== "/oauth/v2/2fa/verify")
      assert.ok(!cookie.includes("mfa-only="));
    if (parsed.pathname === "/oauth/v2/signin" && options.method !== "POST")
      return new Response(
        '<script type="application/json" id="oauth-args">{"csrf-token":"fixture-csrf"}</script>',
      );
    if (parsed.pathname === "/oauth/v2/signin") {
      counts.signin++;
      const fields = new URLSearchParams(options.body);
      assert.equal(fields.get("username"), passwordSecrets.BLINK_EMAIL);
      assert.equal(fields.get("password"), passwordSecrets.BLINK_PASSWORD);
      assert.equal(fields.get("csrf-token"), "fixture-csrf");
      const headers = {
        "set-cookie": "mfa-only=fixture-cookie; Secure; Path=/oauth/v2/2fa",
      };
      return challenge === 202
        ? new Response(
            JSON.stringify({ tsv_state: "pending", tsv_methods: ["sms"] }),
            { status: 202, headers },
          )
        : new Response(null, {
            status: challenge,
            headers: {
              ...headers,
              ...(challenge === 302 ? { Location: "/oauth/v2/authorize" } : {}),
            },
          });
    }
    if (parsed.pathname === "/oauth/v2/2fa/verify") {
      // A standards-based jar preserves the path-restricted cookie between runs.
      assert.match(cookie, /mfa-only=fixture-cookie/);
      counts.verify++;
      if (verifyStatus === "network")
        throw new Error("uncertain fixture-cookie fixture-password 246810");
      const fields = new URLSearchParams(options.body);
      assert.ok(["246810", "135790"].includes(fields.get("2fa_code")));
      assert.equal(fields.get("csrf-token"), "fixture-csrf");
      assert.equal(fields.get("remember_me"), "false");
      return new Response(
        JSON.stringify({
          status: verifyStatus === 201 ? "auth-completed" : "invalid-code",
        }),
        { status: verifyStatus },
      );
    }
    if (parsed.pathname === "/oauth/v2/authorize")
      return new Response(null, {
        status: 302,
        headers: {
          Location:
            "immedia-blink://applinks.blink.com/signin/callback?code=fixture-code",
        },
      });
    assert.equal(parsed.pathname, "/oauth/token");
    counts.exchange++;
    const fields = new URLSearchParams(options.body);
    assert.equal(fields.get("grant_type"), "authorization_code");
    assert.equal(fields.get("code"), "fixture-code");
    assert.equal(
      createHash("sha256")
        .update(fields.get("code_verifier"))
        .digest("base64url"),
      challengeHash,
    );
    return json(tokenPair());
  };
  return {
    counts,
    request,
    setVerifyStatus(value) {
      verifyStatus = value;
    },
  };
}

test("Blink browserless PKCE persists MFA challenges and exchanged tokens without resending signin", async (t) => {
  for (const challenge of [412, 202]) {
    const workspace = await authWorkspace(t);
    const fixture = passwordTranscript({ challenge, failCollection: true });
    const context = { config: blinkConfig, secrets: passwordSecrets };
    await assert.rejects(
      collectBlink(context, fixture.request, workspace.directory),
      errorCode("interaction_required"),
    );
    await assert.rejects(
      collectBlink(context, fixture.request, workspace.directory),
      errorCode("interaction_required"),
    );
    assert.equal(fixture.counts.signin, 1);
    assert.equal(fixture.counts.verify, 0);
    const pending = await workspace.cache();
    assert.ok(pending.pending.cookies);
    assert.ok(!JSON.stringify(pending).includes("fixture-password"));
    await assert.rejects(
      collectBlink(
        {
          ...context,
          secrets: { ...passwordSecrets, BLINK_MFA_CODE: "246810" },
        },
        fixture.request,
        workspace.directory,
      ),
      errorCode("provider_unavailable"),
    );
    assert.equal(fixture.counts.authorize, 1);
    assert.equal(fixture.counts.signin, 1);
    assert.equal(fixture.counts.verify, 1);
    assert.equal(fixture.counts.exchange, 1);
    const cache = await workspace.cache();
    assert.equal(cache.refreshToken, "fixture-refresh-one");
    assert.equal(cache.pending, undefined);
    assert.ok(!JSON.stringify(cache).includes("246810"));
  }
});

test("Blink attempts each MFA secret once, including wrong codes and uncertain verification", async (t) => {
  for (const verifyStatus of [400, "network"]) {
    const workspace = await authWorkspace(t);
    const fixture = passwordTranscript({ verifyStatus });
    const context = {
      config: blinkConfig,
      secrets: { ...passwordSecrets, BLINK_MFA_CODE: "246810" },
    };
    await assert.rejects(
      collectBlink(context, fixture.request, workspace.directory),
      errorCode(
        verifyStatus === "network"
          ? "provider_unavailable"
          : "interaction_required",
      ),
    );
    await assert.rejects(
      collectBlink(context, fixture.request, workspace.directory),
      errorCode("interaction_required"),
    );
    assert.equal(fixture.counts.signin, 1);
    assert.equal(fixture.counts.verify, 1);
    fixture.setVerifyStatus(201);
    const result = await collectBlink(
      { ...context, secrets: { ...passwordSecrets, BLINK_MFA_CODE: "135790" } },
      fixture.request,
      workspace.directory,
    );
    assert.equal(fixture.counts.verify, 2);
    assert.equal(result.data.images.length, 1);
    assert.ok(!JSON.stringify(result).includes("fixture-password"));
  }
});

test("Blink expired MFA sessions require owner recovery without sending another challenge", async (t) => {
  const workspace = await authWorkspace(t);
  const fixture = passwordTranscript();
  const context = { config: blinkConfig, secrets: passwordSecrets };
  await assert.rejects(
    collectBlink(context, fixture.request, workspace.directory),
    errorCode("interaction_required"),
  );
  const cache = await workspace.cache();
  cache.pending.createdAt = Date.now() - 16 * 60_000;
  await writeFile(workspace.path, JSON.stringify(cache));
  await assert.rejects(
    collectBlink(
      { ...context, secrets: { ...passwordSecrets, BLINK_MFA_CODE: "246810" } },
      async () => assert.fail("expired challenge must not send requests"),
      workspace.directory,
    ),
    errorCode("interaction_required"),
  );
});

test("Blink remembers rejected passwords and uncertain signin without repeating scheduled submissions", async (t) => {
  for (const status of [401, null]) {
    const workspace = await authWorkspace(t);
    const fixture = passwordTranscript();
    let signinPosts = 0;
    const request = async (url, options) => {
      if (
        new URL(url).pathname === "/oauth/v2/signin" &&
        options.method === "POST"
      ) {
        signinPosts++;
        if (status === null)
          throw new Error("uncertain fixture-password fixture-cookie");
        return new Response("rejected fixture-password", { status });
      }
      return fixture.request(url, options);
    };
    const context = { config: blinkConfig, secrets: passwordSecrets };
    await assert.rejects(
      collectBlink(context, request, workspace.directory),
      errorCode(status ? "authentication_required" : "provider_unavailable"),
    );
    await assert.rejects(
      collectBlink(context, request, workspace.directory),
      errorCode(status ? "authentication_required" : "interaction_required"),
    );
    assert.equal(signinPosts, 1);
  }
});

test("Blink password fallback follows a revoked refresh token and supports signin without MFA", async (t) => {
  const workspace = await authWorkspace(t);
  const fixture = passwordTranscript({ challenge: 302 });
  let refreshCalls = 0;
  const result = await collectBlink(
    { config: blinkConfig, secrets: { ...refreshSecrets, ...passwordSecrets } },
    async (url, options) => {
      if (
        url === `${oauthOrigin}/oauth/token` &&
        new URLSearchParams(options.body).get("grant_type") === "refresh_token"
      ) {
        refreshCalls++;
        return new Response(null, { status: 401 });
      }
      return fixture.request(url, options);
    },
    workspace.directory,
  );
  assert.equal(refreshCalls, 1);
  assert.equal(fixture.counts.signin, 1);
  assert.equal(fixture.counts.verify, 0);
  assert.equal(result.data.images.length, 1);
});

test("Blink stops unsupported CAPTCHA and external redirects without sending passwords", async (t) => {
  for (const redirect of [false, true]) {
    const workspace = await authWorkspace(t);
    let posts = 0;
    await assert.rejects(
      collectBlink(
        { config: blinkConfig, secrets: passwordSecrets },
        async (url, options) => {
          assert.equal(new URL(url).origin, oauthOrigin);
          if (options.method === "POST") posts++;
          if (redirect)
            return new Response(null, {
              status: 302,
              headers: { Location: "https://security.example/challenge" },
            });
          return new Response("CAPTCHA account security verification");
        },
        workspace.directory,
      ),
      errorCode("interaction_required"),
    );
    assert.equal(posts, 0);
  }
});

test("Blink cancellation preserves a completed rotation and never begins later collection", async (t) => {
  const workspace = await authWorkspace(t);
  const controller = new AbortController();
  let calls = 0;
  await assert.rejects(
    collectBlink(
      {
        config: blinkConfig,
        secrets: refreshSecrets,
        signal: controller.signal,
      },
      async (url) => {
        calls++;
        assert.equal(url, `${oauthOrigin}/oauth/token`);
        controller.abort();
        return json(tokenPair());
      },
      workspace.directory,
    ),
    { name: "AbortError" },
  );
  assert.equal(calls, 1);
  assert.equal((await workspace.cache()).refreshToken, "fixture-refresh-one");
  const nextController = new AbortController();
  const cached = await workspace.cache();
  cached.expiresAt = 0;
  await writeFile(workspace.path, JSON.stringify(cached));
  await assert.rejects(
    collectBlink(
      {
        config: blinkConfig,
        secrets: refreshSecrets,
        signal: nextController.signal,
      },
      async (_url, { signal }) => {
        nextController.abort();
        signal.throwIfAborted();
      },
      workspace.directory,
    ),
    { name: "AbortError" },
  );
  assert.equal((await workspace.cache()).refreshToken, "fixture-refresh-one");
});

function fakeSpawn(action) {
  const child = new EventEmitter();
  child.stdout = new PassThrough();
  child.kill = (signal) => {
    child.killedWith = signal;
    queueMicrotask(() => child.emit("close", null));
    return true;
  };
  const spawnProcess = (path, args, options) => {
    child.invocation = { path, args, options };
    queueMicrotask(() => action?.(child));
    return child;
  };
  return { child, spawnProcess };
}
const rtspOptions = {
  url: "rtsp://fixture:private-password@camera.example/stream",
  ffmpegPath: "ffmpeg",
  transport: "tcp",
  timeoutMs: 100,
  width: 1280,
  quality: 7,
  maximumImageBytes: 1024,
};
const rtspConfig = {
  cameras: [
    { id: "entrance", name: "Entrance", urlSecretName: "RTSP_ENTRANCE_URL" },
  ],
};

test("RTSP captures one JPEG with safe subprocess arguments and no stderr logging", async () => {
  const fixture = fakeSpawn((child) => {
    child.stdout.write(jpeg);
    child.emit("close", 0);
  });
  const result = await collectRtsp(
    { config: rtspConfig, secrets: { RTSP_ENTRANCE_URL: rtspOptions.url } },
    fixture.spawnProcess,
  );
  assert.equal(
    result.data.images[0].dataUrl,
    `data:image/jpeg;base64,${jpeg.toString("base64")}`,
  );
  assert.equal(result.data.motion.available, false);
  assert.equal(fixture.child.invocation.options.shell, false);
  assert.deepEqual(fixture.child.invocation.options.stdio, [
    "ignore",
    "pipe",
    "ignore",
  ]);
  const args = fixture.child.invocation.args;
  assert.equal(args[args.indexOf("-frames:v") + 1], "1");
  assert.equal(args[args.indexOf("-i") + 1], rtspOptions.url);
  assert.equal(args.at(-1), "pipe:1");
  assert.ok(!JSON.stringify(result).includes("private-password"));
});

test("RTSP timeout and excess output kill the child and wait for close", async () => {
  const stalled = fakeSpawn();
  await assert.rejects(
    captureFrame(rtspOptions, undefined, stalled.spawnProcess),
    /timed out/,
  );
  assert.equal(stalled.child.killedWith, "SIGKILL");
  const tooLarge = fakeSpawn((child) => child.stdout.write(Buffer.alloc(1025)));
  await assert.rejects(
    captureFrame(rtspOptions, undefined, tooLarge.spawnProcess),
    /maximumImageBytes/,
  );
  assert.equal(tooLarge.child.killedWith, "SIGKILL");
});

test("RTSP cancellation kills an active capture and rejects pre-aborted work without spawning", async () => {
  const controller = new AbortController();
  const fixture = fakeSpawn(() => controller.abort());
  await assert.rejects(
    captureFrame(rtspOptions, controller.signal, fixture.spawnProcess),
    /cancelled/,
  );
  assert.equal(fixture.child.killedWith, "SIGKILL");
  assert.throws(
    () =>
      captureFrame(rtspOptions, controller.signal, () =>
        assert.fail("must not spawn"),
      ),
    { name: "AbortError" },
  );
});

test("RTSP validates image completeness and masks spawn/decoder failures", async () => {
  const invalid = fakeSpawn((child) => {
    child.stdout.write(Buffer.from([0xff, 0xd8, 0x00]));
    child.emit("close", 0);
  });
  await assert.rejects(
    captureFrame(rtspOptions, undefined, invalid.spawnProcess),
    /complete JPEG/,
  );
  const failed = fakeSpawn((child) => child.emit("close", 1));
  await assert.rejects(
    captureFrame(rtspOptions, undefined, failed.spawnProcess),
    (error) =>
      /FFmpeg capture failed/.test(error.message) &&
      !error.message.includes("private-password"),
  );
  const missing = fakeSpawn((child) =>
    child.emit("error", new Error(rtspOptions.url)),
  );
  await assert.rejects(
    captureFrame(rtspOptions, undefined, missing.spawnProcess),
    /^Error: Cannot start FFmpeg;/,
  );
});

test("RTSP missing or invalid URL secrets do not start FFmpeg", async () => {
  for (const url of [
    undefined,
    "invalid",
    "https://fixture:private-password@camera.example/",
  ]) {
    await assert.rejects(
      collectRtsp(
        { config: rtspConfig, secrets: { RTSP_ENTRANCE_URL: url } },
        () => assert.fail("must not spawn"),
      ),
      (error) =>
        !error.message.includes("private-password") &&
        /URL secret/.test(error.message),
    );
  }
});

test("RTSP preserves successful cameras alongside partial failures", async () => {
  const fixture = fakeSpawn((child) => {
    child.stdout.write(jpeg);
    child.emit("close", 0);
  });
  const result = await collectRtsp(
    {
      config: {
        ...rtspConfig,
        cameras: [
          ...rtspConfig.cameras,
          { id: "garage", urlSecretName: "MISSING_URL" },
        ],
      },
      secrets: { RTSP_ENTRANCE_URL: rtspOptions.url },
    },
    fixture.spawnProcess,
  );
  assert.equal(result.data.images.length, 1);
  assert.equal(result.data.cameras[1].status, "error");
  assert.match(result.data.cameras[1].error, /missing/);
});
