import type { Wire } from '../../contracts/src/generated.js';
/** Best-effort restrictions of the unchanged host, NOT a claim of tool isolation. */
export declare const assessmentToolConfiguration: Record<string, Wire.Json>;
type Parameters = {
    threadStart: Record<string, Wire.Json>;
    threadResume: Record<string, Wire.Json>;
    turnStart: Record<string, Wire.Json>;
};
export declare function restrictAssessmentParameters<T extends Parameters>(original: T): T;
export declare function assertAssessmentPolicy(params: Parameters): void;
export {};
