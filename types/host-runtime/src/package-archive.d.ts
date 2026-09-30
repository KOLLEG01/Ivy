import type { Host } from '../../contracts/src/generated.js';
export interface PackageCatalogEntry {
    componentId: string;
    version: string;
    buildId: string;
    archiveHash: string;
    bytes: number;
    manifest: Host.ReleaseManifest;
    revision: number;
    publishedAt: string;
    publisherPrincipalId: string;
}
export interface PackageCatalog {
    schemaVersion: 1;
    revision: number;
    packages: PackageCatalogEntry[];
}
export interface PackageCatalogIndex {
    schemaVersion: 1;
    revision: number;
    packages: Array<{
        componentId: string;
        version: string;
    }>;
}
export interface PackageUploadAuthorization {
    componentId: string;
    version: string;
    buildId: string;
    archiveHash: string;
    bytes: number;
    manifest: Host.ReleaseManifest;
}
export declare function validatePackageAuthorization(value: unknown): PackageUploadAuthorization;
/** Read only stable catalog identity fields before selecting a bootstrap update.
 * A newer release manifest for another component must not block that update. */
export declare function validatePackageCatalogIndex(value: unknown): PackageCatalogIndex;
export declare function validatePackageCatalog(value: unknown): PackageCatalog;
export declare function createPackageArchive(candidate: Host.Candidate, executables: Record<string, string>): Promise<{
    path: string;
    hash: string;
    bytes: number;
}>;
export declare function validatePackageArchive(archive: string, expectedHash: string, expectedManifest: Host.ReleaseManifest, extractionRoot: string, executables: Record<string, string>): Promise<void>;
