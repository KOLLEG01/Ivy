import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { atomicJson, jsonFile, instanceConfig, configurationPath } from './config.js';
import { HiveClient } from '../../sdk/src/client.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { requireThat, IvyError } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';

export class HealthFile {
  private readonly startedAt = new Date().toISOString();
  private readonly bootId = randomUUID();
  readonly launchId = process.env['IVY_LAUNCH_ID'];
  constructor(readonly config: Host.InstanceConfig) {}
  async write(ready: boolean, details = '', generation: number | null = null): Promise<void> {
    const value: Host.Health = { schemaVersion: 1, instanceId: this.config.instanceId, componentId: this.config.componentId, buildId: this.config.buildId,
      pid: process.pid, bootId: this.bootId, startedAt: this.startedAt, observedAt: new Date().toISOString(), ready, generation, ...(this.launchId ? { launchId: this.launchId } : {}), details: details.slice(0, 4096) };
    validateHost('Health', value); await atomicJson(join(this.config.dataRoot, 'health.json'), value);
  }
  async control(): Promise<Host.RuntimeControl | null> {
    try {
      const value = await jsonFile<Host.RuntimeControl>(join(this.config.dataRoot, 'control.json')); validateHost('RuntimeControl', value);
      return value.instanceId === this.config.instanceId && value.launchId === this.launchId && value.bootId === this.bootId ? value : null;
    } catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error; }
  }
}
export function localHiveEndpoint(config: Pick<Host.InstanceConfig, 'publicBaseUrl' | 'settings'>, docker?: Host.Instance['docker']): URL {
  const settings = config.settings as unknown as Host.HiveSettings; validateHost('HiveSettings', settings);
  // The ingress may bind a Docker bridge address. Health uses a separate local
  // binding so authenticated checks never relax the SDK's remote HTTPS rule.
  const mappings = docker?.ports.filter(port => port.containerPort === settings.listenPort && ['127.0.0.1', '::1', '0.0.0.0', '::'].includes(port.host));
  requireThat(!mappings || mappings.length === 1, 'invalid_arguments', 'Hive health needs exactly one loopback-accessible host port mapping for its listener.');
  const binding = mappings?.[0], hostname = binding?.host ?? settings.listenHost;
  const endpoint = new URL(config.publicBaseUrl); endpoint.protocol = 'http:';
  endpoint.hostname = hostname === '0.0.0.0' ? '127.0.0.1' : hostname === '::' ? '[::1]' : hostname.includes(':') ? '[' + hostname + ']' : hostname;
  endpoint.port = String(binding?.port ?? settings.listenPort); return endpoint;
}
export async function checkHealth(config: Host.InstanceConfig, hiveEndpoint?: string, verifyRegistration = true): Promise<Host.Health> {
  const value = await jsonFile<Host.Health>(join(config.dataRoot, 'health.json')); validateHost('Health', value);
  requireThat(value.ready && value.instanceId === config.instanceId && value.buildId === config.buildId && Date.now() - Date.parse(value.observedAt) < 15_000 && Date.parse(value.observedAt) <= Date.now() + 1000, 'service_not_ready', 'Instance health is stale, mismatched or not ready.');
  if (config.componentId === 'hive') {
    const settings = config.settings as unknown as Host.HiveSettings; validateHost('HiveSettings', settings);
    const endpoint = hiveEndpoint ? new URL(hiveEndpoint) : localHiveEndpoint(config);
    const status = await new HiveClient(endpoint.href, { credential: settings.credentials[0]!.token }).request('system.status', {}, { timeoutMs: 5000 });
    requireThat(status.ready && status.buildId === config.buildId, 'service_not_ready', 'Hive API/storage identity is not ready.');
  } else if (config.credential && verifyRegistration) {
    const node = await new HiveClient(config.publicBaseUrl, { credential: config.credential }).request('serviceNodes.get', { serviceNodeId: config.serviceNodeId }, { timeoutMs: 5000 });
    requireThat(node.connected && node.synced && node.ready && node.buildId === config.buildId, 'service_not_ready', 'The current Hive service registration is not synchronized and ready.');
  }
  return value;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try { const health = await checkHealth(await instanceConfig(configurationPath()), process.env['IVY_HIVE_HEALTH_URL']); process.stdout.write(JSON.stringify({ ok: true, buildId: health.buildId, generation: health.generation }) + '\n'); }
  catch (error) { process.stdout.write(JSON.stringify({ ok: false, code: IvyError.from(error).code }) + '\n'); process.exitCode = 1; }
}
