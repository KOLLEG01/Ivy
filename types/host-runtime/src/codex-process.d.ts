import type { AppServerSettings } from "./codex-home.js";
import { NativeRpc } from "./codex-rpc.js";
import type { NativeRequest, NativeNotification } from "./codex-rpc.js";
import type { Agent } from "../../sdk/src/node.js";
export interface CodexProcessSettings extends AppServerSettings {
    nativeExecutable: string;
    nativeExecutableHash: string;
    nativeVersion: string;
    windowsShell?: Agent.WindowsShell;
}
/** Hidden public app-server transport shared by service clients. Never touches Desktop UI. */
export declare function startCodexProcess(options: {
    settings: CodexProcessSettings;
    artifactRoot: string;
    dataRoot: string;
    clientName: string;
    clientVersion: string;
    hiveCredential?: string;
    onRequest: (request: NativeRequest) => void;
    onNotification: (notification: NativeNotification) => void;
    onClose: (code: string) => void;
}): Promise<{
    rpc: NativeRpc;
    home: string;
    epoch: `${string}-${string}-${string}-${string}-${string}`;
    close: () => Promise<void>;
    completion: Promise<import("./process.js").ProcessResult>;
    mode: "owned-stdio" | "external-proxy";
}>;
