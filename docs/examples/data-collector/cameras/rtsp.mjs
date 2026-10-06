import { spawn } from "node:child_process";

const maximumTotalImageBytes = 650_000;

function integer(value, fallback, minimum, maximum, name) {
  const result = value ?? fallback;
  if (!Number.isInteger(result) || result < minimum || result > maximum)
    throw new Error(
      `${name} must be an integer from ${minimum} to ${maximum}.`,
    );
  return result;
}

// Exported separately so fixtures can exercise process cleanup without a camera.
export function captureFrame(options, signal, spawnProcess = spawn) {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const args = [
      "-hide_banner",
      "-loglevel",
      "quiet",
      "-nostdin",
      "-rtsp_transport",
      options.transport,
      "-rw_timeout",
      String(options.timeoutMs * 1000),
      "-timeout",
      String(options.timeoutMs * 1000),
      "-i",
      options.url,
      "-map",
      "0:v:0",
      "-frames:v",
      "1",
      "-an",
      "-vf",
      `scale=w='min(${options.width},iw)':h=-2`,
      "-threads",
      "1",
      "-c:v",
      "mjpeg",
      "-q:v",
      String(options.quality),
      "-f",
      "image2pipe",
      "pipe:1",
    ];
    let child;
    try {
      child = spawnProcess(options.ffmpegPath, args, {
        shell: false,
        windowsHide: true,
        stdio: ["ignore", "pipe", "ignore"],
      });
    } catch {
      reject(
        new Error(
          "Cannot start FFmpeg; check its installation and executable path.",
        ),
      );
      return;
    }
    let chunks = [],
      size = 0,
      failure;
    const stop = (message) => {
      if (failure) return;
      failure = new Error(message);
      chunks = [];
      child.kill("SIGKILL");
    };
    const onAbort = () => stop("RTSP capture was cancelled.");
    const timer = setTimeout(
      () => stop("RTSP capture timed out."),
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
      size += chunk.length;
      if (size > options.maximumImageBytes)
        stop(
          "RTSP image exceeds maximumImageBytes; reduce width or JPEG quality.",
        );
      else chunks.push(chunk);
    });
    child.once("error", () => {
      cleanup();
      reject(
        new Error(
          "Cannot start FFmpeg; check its installation and executable path.",
        ),
      );
    });
    child.once("close", (code) => {
      cleanup();
      if (failure) reject(failure);
      else if (code !== 0)
        reject(
          new Error(
            "FFmpeg capture failed; check camera access, credentials and stream compatibility.",
          ),
        );
      else {
        const bytes = Buffer.concat(chunks);
        if (
          bytes.length < 4 ||
          bytes[0] !== 0xff ||
          bytes[1] !== 0xd8 ||
          bytes.at(-2) !== 0xff ||
          bytes.at(-1) !== 0xd9
        )
          reject(new Error("FFmpeg did not return a complete JPEG image."));
        else resolve(bytes);
      }
    });
  });
}

export default async function collect(
  { config, secrets, signal },
  spawnProcess = spawn,
) {
  const cameras = config.cameras;
  if (!Array.isArray(cameras) || cameras.length < 1 || cameras.length > 5)
    throw new Error(
      "Configure one to five cameras; one task per camera is recommended.",
    );
  const options = {
    ffmpegPath: config.ffmpegPath ?? "ffmpeg",
    timeoutMs: integer(
      config.captureTimeoutMs,
      10_000,
      100,
      30_000,
      "captureTimeoutMs",
    ),
    width: integer(config.width, 1280, 64, 3840, "width"),
    quality: integer(config.quality, 7, 2, 31, "quality"),
    maximumImageBytes: integer(
      config.maximumImageBytes,
      524_288,
      1024,
      614_400,
      "maximumImageBytes",
    ),
    transport: config.transport ?? "tcp",
  };
  if (
    typeof options.ffmpegPath !== "string" ||
    !options.ffmpegPath ||
    !["tcp", "udp"].includes(options.transport)
  )
    throw new Error(
      "Configure an FFmpeg executable path and tcp or udp transport.",
    );
  const ids = new Set();
  for (const camera of cameras) {
    if (
      !camera ||
      typeof camera.id !== "string" ||
      !/^[a-z0-9][a-z0-9_-]{0,63}$/i.test(camera.id) ||
      ids.has(camera.id)
    )
      throw new Error("Camera ids must be unique short identifiers.");
    if (typeof camera.urlSecretName !== "string" || !camera.urlSecretName)
      throw new Error("Each camera needs urlSecretName.");
    ids.add(camera.id);
  }
  const images = [],
    statuses = [];
  let totalBytes = 0;
  for (const camera of cameras) {
    signal?.throwIfAborted();
    const name = String(camera.name ?? camera.id).slice(0, 200);
    try {
      const url = secrets[camera.urlSecretName];
      if (typeof url !== "string" || !url)
        throw new Error("The selected RTSP URL secret is missing.");
      let parsed;
      try {
        parsed = new URL(url);
      } catch {
        throw new Error("The RTSP URL secret is invalid.");
      }
      if (!["rtsp:", "rtsps:"].includes(parsed.protocol) || !parsed.hostname)
        throw new Error("The URL secret must use rtsp or rtsps.");
      const bytes = await captureFrame(
        { ...options, url },
        signal,
        spawnProcess,
      );
      if (totalBytes + bytes.length > maximumTotalImageBytes)
        throw new Error(
          "Combined images exceed the result budget; use one task per camera.",
        );
      totalBytes += bytes.length;
      const capturedAt = new Date().toISOString();
      images.push({
        id: camera.id,
        name,
        dataUrl: `data:image/jpeg;base64,${bytes.toString("base64")}`,
        capturedAt,
      });
      statuses.push({ id: camera.id, name, status: "ok", capturedAt });
    } catch (error) {
      signal?.throwIfAborted();
      statuses.push({
        id: camera.id,
        name,
        status: "error",
        error: error.message,
      });
    }
  }
  if (images.length === 0)
    throw new Error(
      statuses.map((camera) => `${camera.id}: ${camera.error}`).join("; "),
    );
  return {
    data: {
      images,
      cameras: statuses,
      motion: {
        available: false,
        reason: "RTSP snapshots contain no motion events.",
      },
    },
  };
}
