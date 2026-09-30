import type { Chat, RpcClient } from './client.js';
export declare const chatInterfaceVersion = "1.2.0";
export interface MainChat {
    serviceNodeId: string;
    expectedBridge: Chat.ExpectedBridge;
    channel: Chat.Channel;
}
/** Hive selects the unique ChatBridge. Consumers never configure their own Main destination. */
export declare function mainChat(client: RpcClient): Promise<MainChat>;
/** Transport metadata stays in durable receipts; callers submit only the original service content. */
export declare const mainNotification: ({ action, operationId, source, text }: Chat.NotifyRequest) => Chat.NotifyRequest;
