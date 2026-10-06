import { compareVersions } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';

type RuntimeRequirements = Host.ReleaseManifest['requirements'];

function nodeVersion(value: string): string {
  const normalized = value.startsWith('v') ? value.slice(1) : value;
  requireThat(/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(normalized),
    'unsupported_runtime', 'Node runtime version is not an exact numeric version.');
  return normalized;
}

export function runtimeRequirementsSatisfied(requirements: RuntimeRequirements,
  runtime: { node: string; os: string; arch: string } = { node: process.version, os: process.platform, arch: process.arch }): boolean {
  const match = /^>=(\d+\.\d+\.\d+) <(\d+\.\d+\.\d+)$/.exec(requirements.node);
  if (!match) throw new IvyError('invalid_arguments', 'Package Node requirement is invalid.');
  const current = nodeVersion(runtime.node);
  return compareVersions(current, match[1]!) >= 0 && compareVersions(current, match[2]!) < 0 &&
    (!requirements.os || requirements.os.includes(runtime.os as 'win32' | 'linux')) &&
    (!requirements.arch || requirements.arch.includes(runtime.arch as 'x64' | 'arm64'));
}

export function requireRuntimeRequirements(requirements: RuntimeRequirements): void {
  requireThat(runtimeRequirementsSatisfied(requirements), 'unsupported_runtime',
    `Package does not support ${process.platform}/${process.arch} on ${process.version}.`);
}
