import type { Host } from '../../contracts/src/generated.js';
export declare function processStopFence(dataRoot: string): Promise<Host.ProcessStopFence | null>;
/** Remove only the exact fence whose external owner-release proof just passed. */
export declare function clearProcessStopFence(dataRoot: string, expected: Host.ProcessStopFence): Promise<boolean>;
/** The small stop marker survives restart; only explicit ownership reconciliation clears it. */
export declare function recordProcessStopFence(dataRoot: string, fence: Host.ProcessStopFence): Promise<boolean>;
export declare function requireClearProcessStopFence(dataRoot: string): Promise<void>;
