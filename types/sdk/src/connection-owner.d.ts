import type { RpcClient, Result } from './client.js';
type Owner = Result<'serviceNodes.get'>;
/** Bound ownership, not live status. Authenticate the owner once per ServiceConnection. */
export declare function connectionOwner(client: RpcClient, serviceNodeId: string): Promise<Owner>;
export {};
