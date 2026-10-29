/** Routed writes require their own operation identity; subscriptions use their owning socket. */
export declare function isReadRequest(method: string, params: Record<string, unknown>): boolean;
