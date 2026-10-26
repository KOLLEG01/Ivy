import { lstat, readFile, readdir, realpath } from "node:fs/promises";
import { extname, resolve, sep, basename, dirname } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { canonical, digest, hashJson } from "../../contracts/src/canonical.js";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import { validateUiDefinition } from "../../contracts/src/core-validation.js";
import {
  deriveOperationId,
  parseOperationId,
} from "../../contracts/src/operation-id.js";
import type { Operation, Wire } from "../../contracts/src/generated.js";
import type { RpcClient } from "../../sdk/src/client.js";

const media: Record<string, string> = {
  ".html": "text/html",
  ".css": "text/css",
  ".js": "text/javascript",
  ".json": "application/json",
  ".txt": "text/plain",
  ".md": "text/plain",
  ".wasm": "application/wasm",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".webp": "image/webp",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
};
interface FileAsset {
  path: string;
  mediaType: string;
  contentHash: string;
  byteLength: number;
}

export async function readUiBundle(
  directory: string,
  definition: Operation.UiDefinition,
) {
  validateUiDefinition(definition);
  const root = resolve(directory);
  requireThat(
    (await realpath(root)) === root && !(await lstat(root)).isSymbolicLink(),
    "invalid_arguments",
    "UI bundle must be an actual directory, not a link.",
  );
  const assets: FileAsset[] = [];
  let totalBytes = 0;
  async function walk(relative = "") {
    for (const entry of await readdir(resolve(root, relative), {
      withFileTypes: true,
    })) {
      const path = relative + entry.name,
        absolute = resolve(root, path);
      requireThat(
        absolute.startsWith(root + sep) &&
          !entry.isSymbolicLink() &&
          !/[\\%?#:\u0000-\u001f\u007f]/.test(path) &&
          path === path.normalize("NFC"),
        "invalid_arguments",
        "Bundle paths must be normalized relative files inside the selected directory.",
      );
      if (entry.isDirectory()) {
        await walk(path + "/");
        continue;
      }
      requireThat(
        entry.isFile(),
        "invalid_arguments",
        "Bundle contains a non-file entry.",
      );
      if (path === "ivy-ui.json") continue;
      const mediaType =
        path === "LICENSE" ? "text/plain" : media[extname(path)];
      requireThat(
        mediaType,
        "invalid_arguments",
        "Unsupported static asset extension: " + path,
      );
      const metadata = await lstat(absolute);
      requireThat(
        metadata.size <= 8 * 1024 * 1024,
        "content_too_large",
        "Static asset exceeds the binary limit.",
      );
      const bytes = await readFile(absolute);
      requireThat(
        (await realpath(absolute)) === absolute,
        "invalid_arguments",
        "Asset path changed outside the bundle.",
      );
      let decoded: Uint8Array = bytes;
      if (mediaType === "application/json") {
        const value = JSON.parse(
          new TextDecoder("utf-8", { fatal: true }).decode(bytes),
        ) as Wire.Json;
        decoded = Buffer.from(canonical(value, 1024 * 1024));
      } else if (mediaType.startsWith("text/"))
        new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
      requireThat(
        decoded.byteLength <=
          (mediaType.startsWith("text/") || mediaType === "application/json" ? 1 : 8) * 1024 * 1024,
        "content_too_large",
        "Static asset exceeds its content limit.",
      );
      totalBytes += decoded.byteLength;
      requireThat(
        totalBytes <= 32 * 1024 * 1024 && assets.length < 2000,
        "limit_exceeded",
        "Static bundle exceeds its operating bound.",
      );
      assets.push({
        path,
        mediaType,
        contentHash: digest(decoded),
        byteLength: decoded.byteLength,
      });
    }
  }
  await walk();
  assets.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  requireThat(
    assets.some(
      (asset) =>
        asset.path === definition.entryPath && asset.mediaType === "text/html",
    ),
    "invalid_arguments",
    "The ui needs an HTML entry in its complete bundle.",
  );
  const bundleHash = hashJson({
    definition,
    assets,
  });
  return { assets, bundleHash, releaseId: bundleHash.slice(7) };
}

export async function publishUi(
  client: RpcClient,
  input: {
    directory: string;
    definition: Operation.UiDefinition;
    expectedReleaseId: string | null;
    mutationId: string;
    checkedBundle?: Awaited<ReturnType<typeof readUiBundle>>;
  },
): Promise<Operation.UiPointerResult> {
  parseOperationId(input.mutationId);
  const { definition, mutationId, expectedReleaseId } = input,
    bundle =
      input.checkedBundle ?? (await readUiBundle(input.directory, definition));
  const register = async (value: Wire.DataContract) =>
    client.request("contracts.register", {
      mutationId: deriveOperationId(mutationId, [
        "contract",
        value.key,
        value.version,
      ]),
      definition: value,
    });
  for (const value of definition.dataContracts) {
    requireThat(
      value.owner.kind === "agent",
      "contract_owner_mismatch",
      "UI publication can only register agent-owned data contracts.",
    );
    await register(value);
  }
  const assets: Operation.UiAsset[] = [];
  for (const item of bundle.assets) {
    const asset = {
      path: item.path,
      contentHash: item.contentHash,
      mediaType: item.mediaType,
      byteLength: item.byteLength,
    };
    let bytes = await readFile(resolve(input.directory, item.path));
    if (item.mediaType === "application/json")
      bytes = Buffer.from(canonical(JSON.parse(bytes.toString("utf8"))));
    requireThat(bytes.length === asset.byteLength && digest(bytes) === asset.contentHash,
      "artifact_changed", "UI bundle changed after validation.");
    await client.request("uis.stageAsset", {
      uiId: definition.metadata.uiId,
      releaseId: bundle.releaseId,
      asset,
      base64: bytes.toString("base64"),
      mutationId: deriveOperationId(mutationId, ["asset", item.path, item.contentHash]),
    });
    assets.push(asset);
  }
  return client.request("uis.deploy", {
    mutationId,
    expectedReleaseId,
    metadata: definition.metadata,
    release: {
      releaseId: bundle.releaseId,
      entryPath: definition.entryPath,
      requirements: definition.requirements,
      assets,
    },
  });
}

// Exported publication helpers serve intentional Node SDK/browser consumers.
// The operator command uses the same native implementation as Host recovery.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const moduleRoot = fileURLToPath(new URL('../../../', import.meta.url));
  const root = basename(moduleRoot) === 'dist' ? dirname(moduleRoot) : moduleRoot;
  const executable = resolve(root, 'dist/native', `${process.platform}-${process.arch}`,
    process.platform === 'win32' ? 'ivy-publish-ui.exe' : 'ivy-publish-ui');
  const result = spawnSync(executable, process.argv.slice(2), { stdio: 'inherit', windowsHide: true });
  if (result.error) {
    console.error(JSON.stringify(new IvyError('unsupported_runtime',
      'The native UI publication command is unavailable. Build or install the HostExecutor package.', 'not_executed').toWire()));
  }
  process.exitCode = result.status ?? 1;
}
