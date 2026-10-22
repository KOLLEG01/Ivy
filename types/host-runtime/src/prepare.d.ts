import type { Host } from '../../contracts/src/generated.js';
export declare function prepareCandidate(snapshot: Host.SourceSnapshot, componentId: string, config: Host.HostConfig, bootstrapRoot: string): Promise<Host.Candidate>;
