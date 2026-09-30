import type { Host } from '../../contracts/src/generated.js';
export declare const fileHash: (path: string) => Promise<string>;
/** The only generic archive checksum boundary: bytes arriving from another host. */
export declare function verifyTransferredArchive(path: string, expectedHash: string): Promise<void>;
/** Read the runtime contract shared by retained and newly written manifests.
 * Historical preparation/provenance fields have no launch meaning. */
export declare function releaseManifest(input: unknown): Host.ReleaseManifest;
export declare function readReleaseManifest(path: string): Promise<Host.ReleaseManifest>;
/** Deterministic contained-file inventory used for package and source identity.
 * It is not part of normal candidate activation. */
export declare function artifactFiles(root: string): Promise<Array<{
    path: string;
    hash: string;
    bytes: number;
    mode: number;
}>>;
export declare function verifyCandidate(candidate: Host.Candidate, config: Pick<Host.HostConfig, 'artifactRoot'>): Promise<Host.ReleaseManifest>;
/** A private immutable release needs only its identity stamp and selected entry. */
export declare function verifyLaunchCandidate(candidate: Host.Candidate, config: Pick<Host.HostConfig, 'artifactRoot'>, entrypoint: string | undefined): Promise<void>;
