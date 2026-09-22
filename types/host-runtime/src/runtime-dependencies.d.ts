/** Exact package locations, never package-name or path-prefix guesses. */
export declare function developmentPackageLocations(lock: unknown): ReadonlySet<string>;
/** npm's exact workspace junction/symlink locations and their source directories. */
export declare function workspacePackageLocations(lock: unknown): ReadonlyMap<string, string>;
/** Exact installed package closure for the external specifiers emitted by one component build. */
export declare function runtimePackageLocations(lock: unknown, names: Iterable<string>): ReadonlySet<string>;
