import type { Host } from '../../contracts/src/generated.js';
export declare function verifyBackup(directory: string, expectedHash: string): Promise<Host.BackupManifest>;
export declare function restoreHost(backupRoot: string, manifestHash: string, templatePath: string, distributionRoot: string): Promise<Host.RestoreResult>;
