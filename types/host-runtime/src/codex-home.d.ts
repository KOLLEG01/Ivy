export interface CodexHomeSettings {
    codexHome?: string;
    nativeHome?: string;
}
export interface AppServerSettings extends CodexHomeSettings {
    appServer?: {
        mode: 'owned-stdio';
    } | {
        mode: 'external-proxy';
        socketPath?: string;
        expectedCodexHome?: string;
    };
}
export declare const usesExternalAppServer: (settings: AppServerSettings) => boolean;
export declare function expectedServerHome(settings: AppServerSettings): string;
export interface CodexHomeResolution {
    path: string;
    source: 'host-configuration' | 'existing-native-home' | 'environment' | 'user-home';
}
/** Resolve for the executing account without mutating its environment or moving any data. */
export declare function resolveCodexHome(settings: CodexHomeSettings, options?: {
    platform?: NodeJS.Platform;
    environment?: NodeJS.ProcessEnv;
    userHome?: string;
    cwd?: string;
}): CodexHomeResolution;
/** Ownership comes only from an explicit path within this instance, never from a default. */
export declare function instanceOwnedCodexHome(settings: CodexHomeSettings, dataRoot: string, platform?: NodeJS.Platform): string | null;
