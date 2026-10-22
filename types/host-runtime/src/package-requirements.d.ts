import type { Host } from '../../contracts/src/generated.js';
type RuntimeRequirements = Host.ReleaseManifest['requirements'];
export declare function runtimeRequirementsSatisfied(requirements: RuntimeRequirements, runtime?: {
    node: string;
    os: string;
    arch: string;
}): boolean;
export declare function requireRuntimeRequirements(requirements: RuntimeRequirements): void;
export {};
