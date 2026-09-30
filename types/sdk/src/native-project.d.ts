import type { Agent, Wire } from '../../contracts/src/generated.js';
import type { RpcClient, RequestOptions } from './client.js';
/** Apply a default before journaling. Explicit IDs (including clearing with "") stay authoritative. */
export declare function defaultNativeThreadProject(params: Record<string, Wire.Json>, inventory: Agent.ProjectsResult): Record<string, Wire.Json>;
export declare function nativeThreadProject(client: RpcClient, serviceNodeId: string, params: Record<string, Wire.Json>, options?: RequestOptions): Promise<Record<string, Wire.Json>>;
