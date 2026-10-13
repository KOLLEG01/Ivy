import { ExecutorLock } from './journal.js';
import type { RetentionProgress } from './storage-space.js';
/** Shared leases protect cache links without serializing independent builds. */
export declare function useDependencyCache(stagingRoot: string, key: string): Promise<ExecutorLock>;
export declare function touchDependencyCache(root: string): Promise<void>;
/** Only the latest reusable cache and caches leased by running builds stay local. */
export declare function collectDependencyCaches(stagingRoot: string, progress: RetentionProgress): Promise<void>;
