import type { IncomingMessage, ServerResponse } from "node:http";
import { createHash } from "node:crypto";
import {
  IvyError,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import type { Wire } from "../../../packages/contracts/src/generated.js";

/** The only unauthenticated service call this route permits is token-checked image rendering. */
export async function dashboardImageHttp(
  request: IncomingMessage,
  response: ServerResponse,
  url: URL,
  invoke: (node: string, args: Wire.Json) => Promise<unknown>,
) {
  response.setHeader("Cache-Control", "private, no-cache");
  response.setHeader("Referrer-Policy", "no-referrer");
  response.setHeader("X-Content-Type-Options", "nosniff");
  response.setHeader("Access-Control-Allow-Origin", "*");
  response.setHeader(
    "Access-Control-Expose-Headers",
    "ETag, X-Dashboard-Metadata, X-Dashboard-Refresh-Seconds",
  );
  if (request.method !== "GET" && request.method !== "HEAD") {
    response.writeHead(405, { Allow: "GET, HEAD" });
    response.end();
    return;
  }
  try {
    const params = url.searchParams;
    const allowed = new Set([
      "node",
      "id",
      "token",
      "width",
      "height",
      "colorMode",
    ]);
    requireThat(
      [...params.keys()].every(
        (key) => allowed.has(key) && params.getAll(key).length === 1,
      ),
      "invalid_arguments",
      "Invalid image query parameters.",
    );
    const node = params.get("node"),
      id = params.get("id"),
      token = params.get("token");
    requireThat(
      node && id && node.length <= 200 && id.length <= 200,
      "invalid_arguments",
      "Select a dashboard and its service node.",
    );
    requireThat(
      token && token.length <= 128,
      "unauthenticated",
      "A dashboard image token is required.",
    );
    const args: Record<string, Wire.Json> = { id, token };
    for (const key of ["width", "height"])
      if (params.has(key)) {
        const value = params.get(key)!;
        requireThat(
          /^\d+$/.test(value),
          "invalid_arguments",
          "Invalid image dimensions.",
        );
        args[key] = Number(value);
      }
    if (params.has("colorMode")) args.colorMode = params.get("colorMode")!;
    const image = (await invoke(node, args)) as {
      mimeType: string;
      encoding: string;
      data: string;
      metadata: Record<string, Wire.Json> & { refreshSeconds: number };
    };
    requireThat(
      image?.mimeType === "image/png" &&
        image.encoding === "base64" &&
        typeof image.data === "string" &&
        image.data.length <= 12 * 1024 * 1024 &&
        image.metadata &&
        typeof image.metadata === "object" &&
        !Array.isArray(image.metadata),
      "provider_contract_error",
      "Invalid dashboard image response.",
    );
    const seconds = image.metadata.refreshSeconds;
    requireThat(
      Number.isInteger(seconds) &&
        (seconds === 0 || (seconds >= 5 && seconds <= 86400)),
      "provider_contract_error",
      "Invalid dashboard refresh metadata.",
    );
    const metadata = encodeURIComponent(JSON.stringify(image.metadata));
    requireThat(
      metadata.length <= 8192,
      "provider_contract_error",
      "Dashboard metadata is too large.",
    );
    const png = Buffer.from(image.data, "base64");
    requireThat(
      png.length >= 33 &&
        png.length <= 8 * 1024 * 1024 &&
        png
          .subarray(0, 8)
          .equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])),
      "provider_contract_error",
      "Invalid dashboard PNG.",
    );
    const etag = '"' + createHash("sha256").update(png).digest("hex") + '"';
    response.setHeader("X-Dashboard-Metadata", metadata);
    response.setHeader("X-Dashboard-Refresh-Seconds", seconds);
    response.setHeader("ETag", etag);
    if (
      request.headers["if-none-match"]
        ?.split(",")
        .map((value) => value.trim())
        .includes(etag)
    ) {
      response.writeHead(304);
      response.end();
      return;
    }
    response.writeHead(200, {
      "Content-Type": "image/png",
      "Content-Length": png.length,
    });
    response.end(request.method === "HEAD" ? undefined : png);
  } catch (error) {
    if (response.destroyed) return;
    const code = IvyError.from(error).code;
    const status =
      code === "unauthenticated" || code === "forbidden"
        ? 401
        : code === "not_found" || code === "target_conflict"
          ? 404
          : code === "invalid_arguments"
            ? 400
            : 503;
    response.setHeader("Cache-Control", "no-store");
    if (status === 503) response.setHeader("Retry-After", "5");
    response.writeHead(status, { "Content-Type": "application/json" });
    response.end(
      request.method === "HEAD"
        ? undefined
        : JSON.stringify({
            error: status === 503 ? "image_unavailable" : code,
          }),
    );
  }
}
