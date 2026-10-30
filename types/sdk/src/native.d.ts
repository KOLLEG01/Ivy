import type { RpcClient, RequestOptions } from './client.js';
import { NativeContract } from '../../contracts/src/native-contract.js';
import type { Agent } from '../../contracts/src/generated.js';
export type NativeTarget = Pick<Agent.Status, 'serviceNodeId' | 'hostId' | 'nativeVersion' | 'nativeExecutableHash' | 'catalogHash'>;
export interface NativeContractSnapshot {
    target: NativeTarget;
    epoch: string;
    contract: NativeContract;
}
/** Read one selected native owner without a latest-version lookup or permission to replay calls. */
export declare function readNativeContract(client: RpcClient, target: NativeTarget, options?: RequestOptions): Promise<NativeContractSnapshot>;
