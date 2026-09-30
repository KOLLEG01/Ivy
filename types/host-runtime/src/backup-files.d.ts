import type { Host } from '../../contracts/src/generated.js';
export declare const missing: (error: unknown) => boolean;
export declare function exists(path: string): Promise<boolean>;
export declare function relativeFile(root: string, path: string): string;
export declare function privateDirectory(path: string): Promise<void>;
export declare function syncDirectories(root: string): Promise<void>;
export declare function inventoryFiles(root: string, exclude?: (path: string) => boolean): Promise<Host.BackupFile[]>;
/** Bounded, flushed copy of a regular immutable observation; never overwrite a target file. */
export declare function copyVerified(source: string, destination: string, expected?: Host.BackupFile, preserveMode?: boolean): Promise<Host.BackupFile>;
