import { canonical } from '../../../packages/contracts/src/canonical.js';
import { isUiMetadata, isUiRelease } from '../../../packages/contracts/src/core-validation.js';
import { IvyError, requireThat } from '../../../packages/contracts/src/errors.js';
import type { Operation } from '../../../packages/contracts/src/generated.js';
import { Registry } from './registry.js';
import { catalogPage } from './paging.js';
import { HiveStore } from './store.js';
import type { AuthenticatedContext } from './store.js';
import type { UiFiles } from './ui-files.js';

export type UiMetadata = Operation.UiMetadata;
export type UiRequirements = Operation.UiRequirements;
export type UiRelease = Operation.UiRelease;
const mimeTypes = new Set(['text/html', 'text/css', 'text/javascript', 'text/plain', 'application/javascript', 'application/json', 'application/wasm', 'image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/svg+xml', 'image/x-icon', 'font/woff', 'font/woff2']);
const catalogKey = (ui: Operation.UiSummary) => String(1000000 - (ui.metadata.priority ?? 0)).padStart(7, '0') + ':' + ui.metadata.uiId;

function metadata(uiId: string, encoded: string): UiMetadata | null {
  try {
    const value: unknown = JSON.parse(encoded);
    return isUiMetadata(value) && value.uiId === uiId ? value : null;
  } catch { return null; }
}

export function assetPath(path: string): string {
  requireThat(path.length > 0 && path.length <= 1024 && path.isWellFormed() && path === path.normalize('NFC') && !/[\\%?#:\u0000-\u001f\u007f]/.test(path) && path.split('/').every(part => part.length > 0 && part !== '.' && part !== '..'), 'invalid_arguments', 'Asset path must be a normalized relative path.');
  return path;
}

export class Uis {
  constructor(readonly store: HiveStore, readonly registry: Registry, readonly files: UiFiles) {}
  private pointer(uiId: string) {
    const row = this.store.get('SELECT * FROM apps WHERE app_id=?', uiId);
    requireThat(row, 'not_found', 'UI not found.');
    return { encodedMetadata: String(row['metadata']), currentReleaseId: row['current_release_id'] as string | null };
  }
  private current(uiId: string): { metadata: UiMetadata; currentReleaseId: string | null } | null {
    try {
      const pointer = this.pointer(uiId), value = metadata(uiId, pointer.encodedMetadata);
      return value ? { metadata: value, currentReleaseId: pointer.currentReleaseId } : null;
    } catch (error) { if (error instanceof IvyError && error.code === 'not_found') return null; throw error; }
  }
  private metadata(uiId: string) {
    const current = this.current(uiId);
    requireThat(current, 'not_found', 'UI not found.');
    return current;
  }
  list(): Operation.UI[] {
    return this.store.all('SELECT app_id FROM apps ORDER BY app_id').flatMap(row => {
      const uiId = String(row['app_id']), current = this.current(uiId);
      if (!current) return [];
      return [{ ...current, releases: this.store.all('SELECT release_json FROM app_releases WHERE app_id=? ORDER BY release_id', uiId).flatMap(releaseRow => {
        try { const release: unknown = JSON.parse(String(releaseRow['release_json'])); return isUiRelease(release) ? [release] : []; }
        catch { return []; }
      }) }];
    });
  }
  catalog(params: Operation.UisCatalogParams): Operation.UisCatalogResult {
    const { cursor: _cursor, ...query } = params, identity = { method: 'uis.catalog', ...query };
    let after = params.cursor ? String(this.store.readCursor(identity, params.cursor)) : '';
    const limit = params.limit ?? 50, summaries: Operation.UiSummary[] = [];
    while (summaries.length <= limit) {
      this.store.checkBudget();
      const rows = this.store.all(`WITH navigation AS (SELECT a.*,
        printf('%07d', 1000000 - CASE WHEN json_valid(a.metadata) THEN COALESCE(json_extract(a.metadata,'$.priority'),0) ELSE 0 END) || ':' || a.app_id AS navigation_key
        FROM apps a) SELECT a.app_id,a.metadata,a.current_release_id,a.navigation_key,
        (SELECT COUNT(*) FROM app_releases r WHERE r.app_id=a.app_id) AS release_count
        FROM navigation a WHERE a.navigation_key > ? COLLATE BINARY ORDER BY a.navigation_key LIMIT ?`, after, limit + 1 - summaries.length);
      if (!rows.length) break;
      for (const row of rows) {
        const value = metadata(String(row['app_id']), String(row['metadata']));
        after = String(row['navigation_key']);
        if (value) summaries.push({ metadata: value, currentReleaseId: row['current_release_id'] as string | null, releaseCount: Number(row['release_count']) });
      }
    }
    return catalogPage(this.store, summaries, identity, catalogKey, params);
  }
  resolveSlug(slug: string): string | null {
    const row = this.store.get("SELECT app_id,metadata FROM apps WHERE CASE WHEN json_valid(metadata) THEN json_extract(metadata,'$.slug') END=?", slug);
    return row && metadata(String(row['app_id']), String(row['metadata'])) ? String(row['app_id']) : null;
  }
  releases(params: Operation.UisReleasesParams): Operation.UisReleasesResult {
    this.metadata(params.uiId);
    const { cursor: _cursor, ...query } = params, identity = { method: 'uis.releases', ...query };
    const after = params.cursor ? String(this.store.readCursor(identity, params.cursor)) : '';
    const rows = this.store.all(`SELECT release_id, json_extract(release_json,'$.entryPath') AS entry_path,
      json_array_length(release_json,'$.assets') AS asset_count FROM app_releases
      WHERE app_id=? AND release_id > ? COLLATE BINARY ORDER BY release_id LIMIT ?`, params.uiId, after, (params.limit ?? 50) + 1);
    return catalogPage(this.store, rows.map(row => ({ releaseId: String(row['release_id']), entryPath: String(row['entry_path']), assetCount: Number(row['asset_count']) })), identity, release => release.releaseId, params);
  }
  private serviceStates(registry: Registry, required: Operation.ServiceRequirement) {
    return registry.nodes().filter(node => node.serviceName === required.serviceName).map(node => {
      const namespace = registry.getRegistry(node.serviceNodeId)?.namespaces.find(value => value.namespace === required.namespace);
      return { node, hasCatalog: namespace !== undefined, compatible: !!namespace?.tools.length && namespace.tools.every(tool => tool.interfaceVersion === required.interfaceVersion),
        eligible: registry.eligible(node) };
    });
  }
  inspect(params: Operation.UisInspectParams): Operation.UiInspection {
    const ui = this.pointer(params.uiId), releaseId = params.releaseId ?? ui.currentReleaseId;
    const report: Operation.UiInspection = { uiId: params.uiId, currentReleaseId: ui.currentReleaseId, requestedReleaseId: releaseId,
      release: null, requirements: null, checkedAt: new Date().toISOString(), status: releaseId ? 'ready' : 'unselected', issues: [], issuesTruncated: false };
    const ranks = ['unselected', 'missing', 'invalid', 'incompatible', 'unavailable', 'ready'];
    const issue = (status: Operation.UiInspection['status'], code: string, message: string, resource: Operation.UiIssue['resource'] = {}) => {
      if (ranks.indexOf(status) < ranks.indexOf(report.status)) report.status = status;
      if (report.issues.length < 32) report.issues.push({ code, message, resource }); else report.issuesTruncated = true;
    };
    if (!releaseId) return report;
    const retained = this.store.get('SELECT release_json FROM app_releases WHERE app_id=? AND release_id=?', params.uiId, releaseId);
    if (!retained) { issue('missing', 'not_found', 'The selected release is not retained.'); return report; }
    let release: UiRelease;
    try {
      const decoded: unknown = JSON.parse(String(retained['release_json']));
      requireThat(isUiRelease(decoded), 'ui_release_invalid', 'Release does not match the current ui contract.');
      release = decoded;
      requireThat(release.releaseId === releaseId, 'ui_release_invalid', 'Release identity differs from its retained key.');
      assetPath(release.entryPath);
    } catch (error) {
      if (!(error instanceof IvyError || error instanceof SyntaxError)) throw error;
      issue('invalid', 'ui_release_invalid', 'The retained release manifest is invalid.'); return report;
    }
    report.release = { releaseId, entryPath: release.entryPath, assetCount: release.assets.length }; report.requirements = release.requirements;
    const names = new Set<string>();
    for (const asset of release.assets) {
      try { assetPath(asset.path); } catch { issue('invalid', 'ui_release_invalid', 'An asset path is invalid.', { assetPath: asset.path }); }
      if (names.has(asset.path)) issue('invalid', 'ui_release_invalid', 'An asset path occurs more than once.', { assetPath: asset.path });
      names.add(asset.path);
      if (!mimeTypes.has(asset.mediaType) || !this.files.available(params.uiId, releaseId, asset))
        issue('invalid', 'ui_release_invalid', 'The private UI file is missing or changed.', { assetPath: asset.path });
    }
    if (!release.assets.some(asset => asset.path === release.entryPath && asset.mediaType === 'text/html')) issue('invalid', 'ui_release_invalid', 'The release has no HTML entry asset.', { assetPath: release.entryPath });
    for (const required of release.requirements.contracts) {
      try { this.registry.checkRequirements([required]); }
      catch (error) {
        if (!(error instanceof IvyError) || !['contract_not_found', 'contract_version_conflict'].includes(error.code)) throw error;
        issue('incompatible', error.code, error.message, { contractKey: required.key });
      }
    }
    for (const required of release.requirements.services) {
      const states = this.serviceStates(this.registry, required), resource = { serviceName: required.serviceName, namespace: required.namespace };
      if (!states.length) issue('unavailable', 'service_unavailable', 'The required service has not registered.', resource);
      else if (!states.some(state => state.compatible) && states.some(state => state.hasCatalog))
        issue('incompatible', 'service_interface_changed', 'No provider exposes the required namespace interface version.', resource);
      else if (!states.some(state => state.compatible && state.eligible))
        issue('unavailable', 'service_not_ready', 'No compatible provider currently has an eligible catalog.', resource);
    }
    return report;
  }
  entry(uiId: string): { releaseId: string; entryPath: string } {
    const releaseId = this.pointer(uiId).currentReleaseId;
    requireThat(releaseId, 'not_found', 'UI has no active release.');
    const retained = this.store.get('SELECT release_json FROM app_releases WHERE app_id=? AND release_id=?', uiId, releaseId);
    requireThat(retained, 'not_found', 'Selected UI release is not retained.');
    const release = JSON.parse(String(retained['release_json'])) as UiRelease;
    this.checkServices(release.requirements);
    return { releaseId, entryPath: release.entryPath };
  }
  private checkServices(requirements: UiRequirements): void {
    requireThat(requirements.hiveProtocol === 1, 'unsupported_protocol', 'UI requires an unsupported Hive protocol.');
    for (const required of requirements.services) {
      const states = this.serviceStates(this.registry, required);
      requireThat(states.length > 0, 'service_unavailable', 'Required ui service has not registered.');
      requireThat(states.some(state => state.compatible), 'service_interface_changed', 'Required ui service interface is incompatible.');
      requireThat(states.some(state => state.compatible && state.eligible), 'service_not_ready', 'Required ui service has no eligible compatible provider.');
    }
  }
  get(uiId: string): { metadata: UiMetadata; currentReleaseId: string | null; releases: UiRelease[] } {
    const current = this.metadata(uiId);
    return { ...current,
      releases: this.store.all('SELECT release_json FROM app_releases WHERE app_id=? ORDER BY release_id', uiId).flatMap(row => {
        try { const release: unknown = JSON.parse(String(row['release_json'])); return isUiRelease(release) ? [release] : []; }
        catch { return []; }
      }) };
  }
  stageAsset(context: AuthenticatedContext, params: { uiId: string; releaseId: string; asset: Operation.UiAsset; base64: string; mutationId: string }) {
    return this.store.mutate(context, 'uis.stageAsset', params, () => {
      const bytes = Buffer.from(params.base64, 'base64');
      requireThat(bytes.toString('base64') === params.base64, 'invalid_arguments', 'UI asset encoding is invalid.');
      this.files.stage(params.uiId, params.releaseId, params.asset, bytes);
      return { value: { staged: true } };
    });
  }
  deploy(context: AuthenticatedContext, params: { metadata: UiMetadata; release: UiRelease; expectedReleaseId: string | null; mutationId: string }) {
    return this.store.mutate(context, 'uis.deploy', params, () => {
      const { metadata, release } = params;
      const existing = this.store.get('SELECT current_release_id FROM apps WHERE app_id=?', metadata.uiId);
      const previousReleaseId = existing?.['current_release_id'] ?? null;
      requireThat(previousReleaseId === params.expectedReleaseId, 'release_conflict', 'UI release pointer changed.');
      if (metadata.slug) requireThat(!this.store.get("SELECT app_id FROM apps WHERE app_id<>? AND CASE WHEN json_valid(metadata) THEN json_extract(metadata,'$.slug') END=?", metadata.uiId, metadata.slug), 'invalid_arguments', 'UI path is already registered.');
      this.registry.checkRequirements(release.requirements.contracts, { allowUnregistered: true });
      this.files.promote(metadata.uiId, release.releaseId, release.assets);
      assetPath(release.entryPath);
      const names = release.assets.map(asset => assetPath(asset.path));
      requireThat(new Set(names).size === names.length && names.includes(release.entryPath), 'invalid_arguments', 'UI release needs unique asset paths and an existing entry.');
      for (const asset of release.assets) {
        requireThat(mimeTypes.has(asset.mediaType), 'invalid_arguments', 'UI asset media type is unsupported.');
        requireThat(this.files.check(metadata.uiId, release.releaseId, asset), 'invalid_arguments', 'UI release file does not match its manifest.');
        if (asset.path === release.entryPath) requireThat(asset.mediaType === 'text/html', 'invalid_arguments', 'UI entry must be HTML.');
      }
      const retained = this.store.get('SELECT release_json FROM app_releases WHERE app_id=? AND release_id=?', metadata.uiId, release.releaseId);
      requireThat(!retained || retained['release_json'] === canonical(release), 'release_conflict', 'Release identity already has different immutable assets.');
      if (!existing) this.store.run('INSERT INTO apps VALUES (?,?,NULL,NULL)', metadata.uiId, canonical(metadata));
      if (!retained) {
        this.store.run('INSERT INTO app_releases VALUES (?,?,?)', metadata.uiId, release.releaseId, canonical(release));
      }
      this.store.run('UPDATE apps SET metadata=?,previous_release_id=?,current_release_id=? WHERE app_id=?', canonical(metadata), previousReleaseId, release.releaseId, metadata.uiId);
      this.prune(metadata.uiId);
      this.store.invalidate("uis");
      return { value: { uiId: metadata.uiId, releaseId: release.releaseId, previousReleaseId: previousReleaseId as string | null } };
    });
  }
  rollback(context: AuthenticatedContext, params: { uiId: string; releaseId: string; expectedReleaseId: string; mutationId: string }) {
    return this.store.mutate(context, 'uis.rollback', params, () => {
      const ui = this.get(params.uiId);
      requireThat(ui.currentReleaseId === params.expectedReleaseId, 'release_conflict', 'UI release pointer changed.');
      const release = ui.releases.find(release => release.releaseId === params.releaseId);
      requireThat(release, 'not_found', 'Retained ui release not found.');
      const inspection = this.inspect({ uiId: params.uiId, releaseId: params.releaseId });
      requireThat(inspection.status === 'ready', inspection.issues[0]?.code ?? 'ui_release_invalid', inspection.issues[0]?.message ?? 'Retained UI release is unavailable.');
      this.store.run('UPDATE apps SET previous_release_id=current_release_id,current_release_id=? WHERE app_id=?', params.releaseId, params.uiId);
      this.prune(params.uiId);
      this.store.invalidate("uis");
      return { value: { uiId: params.uiId, releaseId: params.releaseId, previousReleaseId: ui.currentReleaseId } };
    });
  }
  asset(uiId: string, releaseId: string, path: string) {
    assetPath(path);
    const row = this.store.get('SELECT release_json FROM app_releases WHERE app_id=? AND release_id=?', uiId, releaseId);
    requireThat(row, 'not_found', 'Immutable ui release not found.');
    const release = JSON.parse(String(row['release_json'])) as UiRelease;
    const asset = release.assets.find(value => value.path === path);
    requireThat(asset, 'not_found', 'Immutable ui asset not found.');
    return { releaseId, asset };
  }
  currentAsset(uiId: string, path?: string) {
    if (!path) {
      const entry = this.entry(uiId);
      return this.asset(uiId, entry.releaseId, entry.entryPath);
    }
    const releaseId = this.pointer(uiId).currentReleaseId;
    requireThat(releaseId, 'not_found', 'UI has no active release.');
    return this.asset(uiId, releaseId, path);
  }

  private prune(uiId: string): void {
    const pointer = this.store.get('SELECT current_release_id,previous_release_id FROM apps WHERE app_id=?', uiId)!;
    this.store.run('DELETE FROM app_releases WHERE app_id=? AND release_id IS NOT ? AND release_id IS NOT ?',
      uiId, pointer['current_release_id'] ?? null, pointer['previous_release_id'] ?? null);
  }

  collectFiles(): void {
    const retainedByUi = new Map<string, ReadonlySet<string>>();
    for (const row of this.store.all('SELECT app_id FROM apps')) {
      const uiId = String(row['app_id']);
      const retained = new Set(this.store.all('SELECT release_id FROM app_releases WHERE app_id=?', uiId)
        .map(value => String(value['release_id'])));
      retainedByUi.set(uiId, retained);
    }
    this.files.collectAll(retainedByUi);
  }
}
