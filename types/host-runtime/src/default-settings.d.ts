import type { Host } from "../../contracts/src/generated.js";
type Settings = Host.Instance["settings"];
type Context = {
    ivyRoot: string;
    configPath: string;
    join: (...parts: string[]) => string;
};
/** Fill omitted fields only. Explicit nulls, arrays and operator values are authoritative. */
export declare function withDefaults(defaults: Settings, configured: Settings): Settings;
/** Portable service policy; account identities, credentials and devices are installation inputs. */
export declare function defaultServiceSettings(instance: Host.Instance, context: Context): Settings;
export {};
