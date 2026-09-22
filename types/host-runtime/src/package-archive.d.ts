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
export interface PackageUploadAuthorization {
    componentId: string;
    version: string;
    buildId: string;
    archiveHash: string;
    bytes: number;
    manifest: Host.ReleaseManifest;
}
export declare function validatePackageAuthorization(value: unknown): PackageUploadAuthorization;
export declare function validatePackageCatalog(value: unknown): PackageCatalog;
export declare function createPackageArchive(candidate: Host.Candidate, executables: Record<string, string>): Promise<{
    path: string;
    hash: string;
    bytes: number;
}>;
export declare function validatePackageArchive(archive: string, expectedHash: string, expectedManifest: Host.ReleaseManifest, extractionRoot: string, executables: Record<string, string>): Promise<void>;
