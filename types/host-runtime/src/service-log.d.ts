/** Bounded process diagnostics only; permanent work/data are never log-retention targets. */
export declare function serviceLog(root: string): (stream: 'stdout' | 'stderr', text: string) => void;
