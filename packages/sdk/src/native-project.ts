import { posix, win32 } from 'node:path';
import { requireThat } from '../../contracts/src/errors.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import type { Agent, Wire } from '../../contracts/src/generated.js';
import { serviceTools } from './client.js';
import type { RpcClient, RequestOptions } from './client.js';

/** Compare paths on the selected host, including native Windows extended paths. */
function pathKey(value: string): string {
  const path = value.replace(/^\\\\\?\\UNC\\/i, '\\\\').replace(/^\\\\\?\\/, '');
  const windows = /^[a-z]:[\\/]|^\\\\/i.test(path), paths = windows ? win32 : posix;
  requireThat(paths.isAbsolute(path), 'native_project_path_invalid', 'Native project paths must be absolute on their owning host.');
  const key = paths.normalize(path).replace(/[\\/]+$/, '') || '/';
  return windows ? key.toLowerCase().replace(/\\/g, '/') : key;
}
const contains = (root: string, cwd: string) => cwd === root || cwd.startsWith(root.endsWith('/') ? root : root + '/');

/** Apply a default before journaling. An explicit empty ID opts out; native start represents it as null. */
export function defaultNativeThreadProject(params: Record<string, Wire.Json>, inventory: Agent.ProjectsResult): Record<string, Wire.Json> {
  const result = structuredClone(params);
  if (result['projectId'] === '') return { ...result, projectId: null };
  if (result['projectId'] != null || result['ephemeral'] === true) return result;
  const cwd = result['cwd'] ?? inventory.defaults?.internalProjectRoot;
  requireThat(typeof cwd === 'string', 'native_internal_project_missing', 'A service task requires an internal project directory or an explicit working directory.');
  const key = pathKey(cwd);
  const match = inventory.projects.filter(project => project.source === 'native')
    .flatMap(project => project.paths.map(path => ({ id: project.nativeId, path: pathKey(path) })))
    .filter(project => contains(project.path, key)).sort((a, b) => b.path.length - a.path.length)[0];
  if (match) return { ...result, cwd, projectId: match.id };
  const internal = inventory.defaults?.internalProjectRoot;
  requireThat(!internal || !contains(pathKey(internal), key), 'native_internal_project_missing',
    'Register the configured internal project root with Codex before starting service tasks.');
  return result;
}

export async function nativeThreadProject(client: RpcClient, serviceNodeId: string, params: Record<string, Wire.Json>, options?: RequestOptions): Promise<Record<string, Wire.Json>> {
  if (params['projectId'] === '') return { ...structuredClone(params), projectId: null };
  if (params['projectId'] != null || params['ephemeral'] === true) return structuredClone(params);
  const inventory = await serviceTools(client, serviceNodeId, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).read('agent.projects', {}, options);
  validateAgent('ProjectsResult', inventory);
  return defaultNativeThreadProject(params, inventory as Agent.ProjectsResult);
}
