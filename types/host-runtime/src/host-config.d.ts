import type { Host } from '../../contracts/src/generated.js';
export declare function hostConfig(path: string): Promise<Host.HostConfig>;
/** Validate a remotely supplied complete config against the local installation
 * before replacing the durable JSON file. */
export declare function checkedHostConfig(input: Host.HostConfigInput, configurationPath: string): Promise<Host.HostConfig>;
