/** Complete canonical UTF-8 management/transport document, including envelope. Decision0047. */
export declare const managementFrameBytes = 33554432;
/** Canonical UTF-8 bytes admitted for one JSON Object content value. */
export declare const jsonObjectContentBytes = 1048576;
/** UTF-8 bytes admitted for one text Object content value. */
export declare const textObjectContentBytes = 1048576;
/** Decoded bytes admitted for one binary Object before base64 expansion. */
export declare const binaryObjectContentBytes = 8388608;
/** Concurrent requests admitted for one HTTP, WebSocket, or SDK connection. */
export declare const connectionInFlightRequests = 64;
/** Mutation outcomes retained for the complete 24-hour replay window. */
export declare const mutationReceiptCount = 100000;
/** Total encoded mutation-journal storage retained across all principals. */
export declare const mutationReceiptJournalBytes: number;
/** Maximum canonical result retained by one mutation receipt. */
export declare const mutationReceiptResultBytes = 65536;
/** Complete serialized MCP tools/list result, including cursors and cache metadata. */
export declare const mcpDiscoveryResultBytes = 8192;
