import { randomUUID } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  renameSync,
  rmSync,
  rmdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { lstat, mkdir, readFile, realpath, rename, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve, sep } from "node:path";
import { canonical, digest } from "../../../packages/contracts/src/canonical.js";
import { requireThat } from "../../../packages/contracts/src/errors.js";
import type { Operation } from "../../../packages/contracts/src/generated.js";
import type { HiveStore } from "./store.js";
import { assetPath } from "./uis.js";

type Asset = Operation.UiAsset;

const componentId = (value: string) => {
  requireThat(/^[a-z][a-z0-9-]{0,127}$/.test(value), "invalid_arguments", "UI id is invalid.");
  return value;
};
const releaseId = (value: string) => {
  requireThat(/^[a-zA-Z0-9_-]{1,128}$/.test(value), "invalid_arguments", "UI release id is invalid.");
  return value;
};

export class UiFiles {
  constructor(readonly root: string) {}

  directory(uiId: string, release: string): string {
    const path = resolve(this.root, componentId(uiId), releaseId(release));
    requireThat(path.startsWith(resolve(this.root) + sep), "invalid_arguments", "UI directory is invalid.");
    return path;
  }

  path(uiId: string, release: string, relative: string): string {
    const directory = this.directory(uiId, release);
    const path = resolve(directory, assetPath(relative));
    requireThat(path.startsWith(directory + sep), "invalid_arguments", "UI asset path is invalid.");
    return path;
  }

  available(uiId: string, release: string, asset: Asset): boolean {
    try {
      const file = this.path(uiId, release, asset.path);
      const directory = realpathSync(this.directory(uiId, release));
      const info = lstatSync(file);
      return info.isFile() && realpathSync(file).startsWith(directory + sep) &&
        info.size === asset.byteLength;
    } catch {
      return false;
    }
  }

  check(uiId: string, release: string, asset: Asset): boolean {
    return this.available(uiId, release, asset) &&
      digest(readFileSync(this.path(uiId, release, asset.path))) === asset.contentHash;
  }

  private stagingDirectory(uiId: string, release: string): string {
    return resolve(this.root, ".staging", componentId(uiId), releaseId(release));
  }

  stage(uiId: string, release: string, asset: Asset, bytes: Buffer): void {
    requireThat(bytes.length === asset.byteLength && digest(bytes) === asset.contentHash,
      "artifact_changed", "Staged UI file differs from its descriptor.");
    const directory = this.stagingDirectory(uiId, release);
    const file = resolve(directory, assetPath(asset.path));
    requireThat(file.startsWith(directory + sep), "invalid_arguments", "UI asset path is invalid.");
    mkdirSync(dirname(file), { recursive: true });
    if (existsSync(file)) {
      requireThat(digest(readFileSync(file)) === asset.contentHash,
        "mutation_conflict", "Staged UI file already has other content.");
      return;
    }
    const temporary = file + "." + randomUUID() + ".partial";
    try {
      writeFileSync(temporary, bytes, { flag: "wx" });
      renameSync(temporary, file);
    } finally {
      rmSync(temporary, { force: true });
    }
  }

  promote(uiId: string, release: string, assets: readonly Asset[]): void {
    if (assets.every((asset) => this.check(uiId, release, asset))) return;
    const source = this.stagingDirectory(uiId, release);
    for (const asset of assets) {
      const file = resolve(source, assetPath(asset.path));
      requireThat(file.startsWith(source + sep) && existsSync(file) &&
        digest(readFileSync(file)) === asset.contentHash && statSync(file).size === asset.byteLength,
        "ui_release_invalid", "Staged UI release is incomplete.");
    }
    const target = this.directory(uiId, release);
    requireThat(!existsSync(target), "ui_release_invalid", "Existing UI release files are incomplete.");
    mkdirSync(dirname(target), { recursive: true });
    renameSync(source, target);
  }

  async read(uiId: string, release: string, asset: Asset): Promise<Buffer> {
    const file = this.path(uiId, release, asset.path);
    const directory = await realpath(this.directory(uiId, release));
    requireThat((await lstat(file)).isFile() && (await realpath(file)).startsWith(directory + sep),
      "ui_release_invalid", "UI asset leaves its private release directory.");
    const bytes = await readFile(file);
    requireThat(bytes.length === asset.byteLength && digest(bytes) === asset.contentHash,
      "ui_release_invalid", "UI asset differs from its release manifest.");
    return bytes;
  }

  async install(source: string, uiId: string, release: string, assets: readonly Asset[]): Promise<void> {
    const target = this.directory(uiId, release);
    if (assets.every((asset) => this.check(uiId, release, asset))) return;
    requireThat(!existsSync(target), "ui_release_invalid", "Existing UI release files are incomplete.");
    const staging = resolve(this.root, ".incoming", componentId(uiId) + "-" + releaseId(release) + "-" + randomUUID());
    requireThat(staging.startsWith(resolve(this.root, ".incoming") + sep),
      "invalid_arguments", "UI staging directory is invalid.");
    await mkdir(staging, { recursive: true });
    try {
      for (const asset of assets) {
        const relative = assetPath(asset.path);
        const file = resolve(staging, relative);
        requireThat(file.startsWith(staging + sep), "invalid_arguments", "UI asset path is invalid.");
        let bytes = await readFile(resolve(source, relative));
        if (asset.mediaType === "application/json") bytes = Buffer.from(canonical(JSON.parse(bytes.toString("utf8"))));
        requireThat(bytes.length === asset.byteLength && digest(bytes) === asset.contentHash,
          "artifact_changed", "UI bundle changed after validation.");
        await mkdir(dirname(file), { recursive: true });
        await writeFile(file, bytes, { flag: "wx" });
      }
      await mkdir(dirname(target), { recursive: true });
      await rename(staging, target);
    } finally {
      await rm(staging, { recursive: true, force: true });
    }
  }

  collect(uiId: string, retained: ReadonlySet<string>, now = Date.now()): void {
    const directory = resolve(this.root, componentId(uiId));
    if (!existsSync(directory)) return;
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (!entry.isDirectory() || retained.has(entry.name)) continue;
      releaseId(entry.name);
      const target = resolve(directory, entry.name);
      requireThat(target.startsWith(directory + sep), "storage_invalid", "UI release directory is invalid.");
      if (statSync(target).mtimeMs <= now - 24 * 60 * 60 * 1000)
        rmSync(target, { recursive: true, force: true });
    }
    if (retained.size === 0 && readdirSync(directory).length === 0) rmdirSync(directory);
  }

  collectAll(retained: ReadonlyMap<string, ReadonlySet<string>>): void {
    if (!existsSync(this.root)) return;
    for (const entry of readdirSync(this.root, { withFileTypes: true })) {
      if (!entry.isDirectory() || entry.name.startsWith(".")) continue;
      componentId(entry.name);
      this.collect(entry.name, retained.get(entry.name) ?? new Set());
    }
    this.collectTemporary();
  }

  collectTemporary(now = Date.now()): void {
    const cutoff = now - 24 * 60 * 60 * 1000;
    const sweep = (directory: string, depth: number) => {
      if (!existsSync(directory)) return;
      for (const entry of readdirSync(directory, { withFileTypes: true })) {
        if (!entry.isDirectory()) continue;
        const target = resolve(directory, entry.name);
        requireThat(target.startsWith(directory + sep), "storage_invalid", "UI staging path is invalid.");
        if (depth > 0) {
          sweep(target, depth - 1);
          if (readdirSync(target).length === 0) rmdirSync(target);
        }
        else if (statSync(target).mtimeMs < cutoff) rmSync(target, { recursive: true, force: true });
      }
    };
    sweep(resolve(this.root, ".incoming"), 0);
    sweep(resolve(this.root, ".staging"), 1);
  }

  /** One-time conversion of the current rollout's object-backed UI releases. */
  migrateLegacy(store: HiveStore): void {
    if (store.metadata("ui_files_v1") === "ready") return;
    mkdirSync(this.root, { recursive: true });
    const updates: { uiId: string; releaseId: string; json: string }[] = [];
    for (const row of store.all("SELECT app_id,release_id,release_json FROM app_releases")) {
      const uiId = String(row["app_id"]), id = String(row["release_id"]);
      let release: (Omit<Operation.UiRelease, "assets"> & {
        assets: Array<Asset & { objectId?: string; revision?: number }>;
      }) | null = null;
      try { release = JSON.parse(String(row["release_json"])); } catch { continue; }
      if (!release || !Array.isArray(release.assets)) continue;
      if (!release.assets.some((asset) => asset.objectId)) continue;
      const target = this.directory(uiId, id);
      const assets: Asset[] = [];
      for (const old of release.assets) {
        requireThat(old.objectId && Number.isSafeInteger(old.revision), "storage_invalid", "Legacy UI asset is invalid.");
        const stored = store.get(
          `SELECT r.content,r.encoding,r.content_hash,r.byte_length,c.media_type FROM revisions r
             JOIN contract_families c ON c.key=r.contract_key WHERE r.object_id=? AND r.revision=?`,
          old.objectId, old.revision!,
        );
        requireThat(stored && stored["content_hash"] === old.contentHash && stored["media_type"] === old.mediaType,
          "storage_invalid", "Legacy UI asset is missing or changed.");
        const bytes = stored["encoding"] === "base64"
          ? Buffer.from(stored["content"] as Uint8Array)
          : Buffer.from(String(stored["content"]));
        requireThat(digest(bytes) === old.contentHash && bytes.length === Number(stored["byte_length"]),
          "storage_invalid", "Legacy UI asset checksum is invalid.");
        const asset = { path: assetPath(old.path), mediaType: old.mediaType,
          contentHash: old.contentHash, byteLength: bytes.length };
        const file = this.path(uiId, id, asset.path);
        mkdirSync(dirname(file), { recursive: true });
        if (existsSync(file)) requireThat(this.check(uiId, id, asset),
          "storage_invalid", "Existing migrated UI asset differs.");
        else {
          const temporary = file + "." + randomUUID() + ".partial";
          try {
            writeFileSync(temporary, bytes, { flag: "wx" });
            renameSync(temporary, file);
          } finally {
            rmSync(temporary, { force: true });
          }
        }
        for (const entry of readdirSync(dirname(file), { withFileTypes: true })) {
          if (entry.isFile() && entry.name.startsWith(basename(file) + ".") &&
            /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.partial$/.test(entry.name.slice(basename(file).length + 1)))
            rmSync(join(dirname(file), entry.name));
        }
        assets.push(asset);
      }
      requireThat(assets.every((asset) => this.check(uiId, id, asset)) && existsSync(target),
        "storage_invalid", "Migrated UI release is incomplete.");
      updates.push({ uiId, releaseId: id, json: canonical({ ...release, assets }) });
    }
    store.transaction(() => {
      for (const update of updates) store.run(
        "UPDATE app_releases SET release_json=? WHERE app_id=? AND release_id=?",
        update.json, update.uiId, update.releaseId,
      );
      store.run("DELETE FROM app_assets");
      const objects = store.all("SELECT id,contract_key FROM objects WHERE contract_key LIKE 'ui/asset-%' ORDER BY contract_key");
      const removeObject = (row: (typeof objects)[number]) => {
        const usage = store.get("SELECT COUNT(*) AS revisions,COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE object_id=?", row["id"]!)!;
        store.run("DELETE FROM object_fts WHERE object_id=?", row["id"]!);
        store.run("DELETE FROM revisions WHERE object_id=?", row["id"]!);
        store.run("DELETE FROM objects WHERE id=?", row["id"]!);
        store.adjustRetentionUsage(String(row["contract_key"]), -1, -Number(usage["revisions"]), -Number(usage["bytes"]));
      };
      for (const row of objects.filter((value) => value["contract_key"] !== "ui/asset-owner")) {
        removeObject(row);
      }
      for (const row of objects.filter((value) => value["contract_key"] === "ui/asset-owner")) {
        removeObject(row);
      }
      store.setMetadata("ui_files_v1", "ready");
    });
  }
}
