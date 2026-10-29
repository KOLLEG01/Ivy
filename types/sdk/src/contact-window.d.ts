import type { RpcClient, RequestOptions } from './client.js';
export interface ContactWindow {
    timeZone: string;
    start: string;
    end: string;
}
/** Compare local wall-clock time, including windows crossing midnight and DST. */
export declare function contactWindowOpen(rule: {
    window: ContactWindow | null;
}, now: Date): boolean;
/** Live Hive preferences are authoritative for automatic contact by every client. */
export declare function automaticNotificationsAllowed(client: RpcClient, now: Date, options?: RequestOptions): Promise<boolean>;
