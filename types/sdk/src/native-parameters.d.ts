import { NativeContract } from '../../contracts/src/native-contract.js';
import type { Agent, Wire } from '../../contracts/src/generated.js';
import type { RpcClient, RequestOptions } from './client.js';
/** Bounded validated parameters live directly in the owning local workflow; no parameter Object is published. */
export declare function saveNativeParameters(_client: RpcClient, contract: NativeContract, input: {
    method: string;
    params: Wire.Json;
    reserved?: readonly string[];
    parentId: string | null;
    mutationId: string;
}, options?: RequestOptions): Promise<Agent.ParametersRef>;
export declare function readNativeParameters(_client: RpcClient, contract: NativeContract, input: {
    method: string;
    reserved?: readonly string[];
    parentId: string | null;
    ref: Agent.ParametersRef;
}, options?: RequestOptions): Promise<Wire.Json>;
