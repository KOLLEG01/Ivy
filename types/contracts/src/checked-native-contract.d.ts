import { NativeContract } from './native-contract.js';
import type { Agent, Wire } from './generated.js';
interface CheckedNativeContract {
    catalog: Agent.Catalog;
    catalogHash: string;
    definitions: ReadonlyMap<string, Wire.ToolDefinition>;
    contract: NativeContract;
}
export declare function nativeCatalogPath(root: string, version: string, platform?: NodeJS.Platform, architecture?: string): string;
/** Offline evidence uses the exact retained catalog; live admission uses the SDK owner snapshot. */
export declare function checkedNativeContract(version: string, identity?: {
    sourceHash?: string;
    catalogHash?: string;
    nativeExecutableHash?: string;
}): CheckedNativeContract;
export {};
