import type { Transport, Wire } from './generated.js';
/** The provider receives already parsed payloads. Validate only correlation and routing fields. */
export declare function validateProviderCallFrame(value: unknown): asserts value is Transport.ProviderCall;
/** Provider notifications are transient and their payload was already parsed by the socket. */
export declare function validateServiceNotificationFrame(value: unknown): asserts value is Transport.ServiceNotification;
/** Hive adds the authenticated provider identity before delivering a subscribed notification. */
export declare function validateProviderNotificationFrame(value: unknown): asserts value is Transport.ProviderNotification;
/** Lossy event availability is only a wake-up hint for the durable read/ack path. */
export declare function validateEventAvailabilityFrame(value: unknown): asserts value is Transport.EventAvailability;
export type ToolRouting = Wire.ToolCall | {
    serviceName: string;
    qualifiedName: string;
    expectedDefinitionHash: string;
    serviceNodeId?: string;
    arguments: unknown;
    operationId?: string;
    expectedCallerPrincipalId?: string;
};
/** Tool payloads are checked once by the selected domain schema when debug validation is enabled. */
export declare function validateToolRouting(value: unknown, discovery: boolean): asserts value is ToolRouting;
