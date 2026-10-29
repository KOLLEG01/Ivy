import type { RpcClient } from "./client.js";
import type { BrowserNotice } from "../../contracts/src/browser-notice.js";
export { browserNoticeTopic } from "../../contracts/src/browser-notice.js";
export type { BrowserNotice } from "../../contracts/src/browser-notice.js";
/** Register browserNoticeTopic(namespace), then publish through the service's connection. */
export declare function publishBrowserNotice(client: RpcClient, namespace: string, notice: BrowserNotice): Promise<void>;
