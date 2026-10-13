import type { Host } from "../../contracts/src/generated.js";
type SnapshotManifest = Host.SnapshotManifest;
export declare function verifySnapshotMetadata(snapshot: Host.SourceSnapshot, config: Host.HostConfig): Promise<SnapshotManifest>;
export declare function verifySnapshot(snapshot: Host.SourceSnapshot, config: Host.HostConfig): Promise<void>;
export declare function sourceBuildPlan(root: string, componentId: string): Promise<Host.BuildPlan>;
export declare function captureSource(original: string, config: Host.HostConfig): Promise<Host.SourceSnapshot>;
export {};
