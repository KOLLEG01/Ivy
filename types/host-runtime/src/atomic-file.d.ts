/** Preserve the old destination while Windows readers temporarily deny FILE_SHARE_DELETE. */
export declare function replaceFile(temporary: string, destination: string): Promise<void>;
