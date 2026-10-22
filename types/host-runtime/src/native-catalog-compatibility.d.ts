import type { RpcClient } from '../../sdk/src/client.js';
import type { Agent } from '../../contracts/src/generated.js';
/** Catch a pinned executable/catalog mismatch before replacing the native owner's process. */
export declare function verifyConfiguredNativeCatalog(artifactRoot: string, instanceId: string, settings: Pick<Agent.Settings, 'nativeVersion' | 'nativeExecutableHash'>): Promise<void>;
/** Read metadata only, before draining the current service. Offline hosts do not block deployment. */
export declare function verifyNativeCatalogCompatibility(client: RpcClient, artifactRoot: string, componentId: string): Promise<void>;
