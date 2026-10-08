import { requireThat } from './errors.js';
import type { Transport, Wire } from './generated.js';

const object = (value: unknown, message: string): Record<string, unknown> => {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'invalid_frame', message);
  return value as Record<string, unknown>;
};
const text = (value: unknown, maximum = 256): value is string => typeof value === 'string' && value.length > 0 && value.length <= maximum;
const identifier = (value: unknown): value is string => text(value, 256);
const hash = (value: unknown): value is string => typeof value === 'string' && /^sha256:[a-f0-9]{64}$/.test(value);
const optionalText = (value: unknown): boolean => value === undefined || text(value);
const serviceName = (value: unknown): value is string => typeof value === 'string' && value.length <= 64 && /^[a-z][a-z0-9-]*$/.test(value);
const contractVersion = (value: unknown): value is string => typeof value === 'string' && /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(value);
const exactKeys = (value: Record<string, unknown>, allowed: readonly string[]): boolean => Object.keys(value).every(key => allowed.includes(key));
const resourceRef = (value: Record<string, unknown>): boolean =>
  exactKeys(value, ['serviceNodeId', 'namespace', 'kind', 'nativeId']) && identifier(value['serviceNodeId']) &&
  text(value['namespace'], 64) && text(value['kind'], 64) && text(value['nativeId'], 1024);

/** The provider receives already parsed payloads. Validate only correlation and routing fields. */
export function validateProviderCallFrame(value: unknown): asserts value is Transport.ProviderCall {
  const frame = object(value, 'Expected one provider request envelope.');
  requireThat(frame['jsonrpc'] === '2.0' && frame['method'] === 'provider.invoke' && identifier(frame['id']) &&
    exactKeys(frame, ['jsonrpc', 'id', 'method', 'params']), 'invalid_frame', 'Provider request correlation is invalid.');
  const params = object(frame['params'], 'Provider request params must be an object.');
  requireThat(exactKeys(params, ['qualifiedName', 'arguments', 'definitionHash', 'generation', 'callerPrincipalId', 'operationId']) &&
    text(params['qualifiedName']) && Object.hasOwn(params, 'arguments') && hash(params['definitionHash']) &&
    Number.isSafeInteger(params['generation']) && Number(params['generation']) >= 1 && identifier(params['callerPrincipalId']) &&
    (params['operationId'] === undefined || identifier(params['operationId'])),
  'invalid_frame', 'Provider request routing fields are invalid.');
}

/** Provider notifications are transient and their payload was already parsed by the socket. */
export function validateServiceNotificationFrame(value: unknown): asserts value is Transport.ServiceNotification {
  const frame = object(value, 'Expected one provider notification envelope.');
  requireThat(frame['jsonrpc'] === '2.0' && frame['method'] === 'service.notification' && exactKeys(frame, ['jsonrpc', 'method', 'params']),
    'invalid_frame', 'Provider notification envelope is invalid.');
  const params = object(frame['params'], 'Provider notification params must be an object.');
  requireThat(exactKeys(params, ['namespace', 'name', 'version', 'payload']) && serviceName(params['namespace']) && text(params['name'], 192) &&
    contractVersion(params['version']) && Object.hasOwn(params, 'payload'), 'invalid_frame', 'Provider notification routing fields are invalid.');
}

/** Hive adds the authenticated provider identity before delivering a subscribed notification. */
export function validateProviderNotificationFrame(value: unknown): asserts value is Transport.ProviderNotification {
  const frame = object(value, 'Expected one subscribed provider notification envelope.');
  requireThat(frame['jsonrpc'] === '2.0' && frame['method'] === 'notifications.provider' && exactKeys(frame, ['jsonrpc', 'method', 'params']),
    'invalid_frame', 'Subscribed provider notification envelope is invalid.');
  const params = object(frame['params'], 'Subscribed provider notification params must be an object.');
  requireThat(exactKeys(params, ['namespace', 'name', 'version', 'payload', 'serviceNodeId', 'generation']) &&
    serviceName(params['namespace']) && text(params['name'], 192) && contractVersion(params['version']) &&
    Object.hasOwn(params, 'payload') && identifier(params['serviceNodeId']) && Number.isSafeInteger(params['generation']) && Number(params['generation']) >= 1,
  'invalid_frame', 'Subscribed provider notification routing fields are invalid.');
}

/** UI invalidations carry no data or delivery authority; reconnect rereads current state. */
export function validateChangeNotificationFrame(value: unknown): asserts value is Transport.ChangeNotification {
  const frame = object(value, 'Expected one UI invalidation envelope.');
  requireThat(frame['jsonrpc'] === '2.0' && frame['method'] === 'notifications.changed' &&
    exactKeys(frame, ['jsonrpc', 'method', 'params']), 'invalid_frame', 'UI invalidation envelope is invalid.');
  const params = object(frame['params'], 'UI invalidation params must be an object.');
  const scopes = params['scopes'];
  requireThat(exactKeys(params, ['scopes']) && Array.isArray(scopes) && scopes.length <= 64 &&
    new Set(scopes).size === scopes.length && scopes.every(scope => typeof scope === 'string' && scope.length <= 512 &&
      /^((?:objects|services|inventory|system)(?:\/[a-zA-Z0-9._/-]+)?|uis)$/.test(scope)),
    'invalid_frame', 'UI invalidation scopes are invalid.');
}

/** Lossy event availability is only a wake-up hint for the durable read/ack path. */
export function validateEventAvailabilityFrame(value: unknown): asserts value is Transport.EventAvailability {
  const frame = object(value, 'Expected one event availability envelope.');
  requireThat(frame['jsonrpc'] === '2.0' && frame['method'] === 'events.available' && exactKeys(frame, ['jsonrpc', 'method', 'params']),
    'invalid_frame', 'Event availability envelope is invalid.');
  const params = object(frame['params'], 'Event availability params must be an object.');
  requireThat(exactKeys(params, ['throughSequence']) && Number.isSafeInteger(params['throughSequence']) && Number(params['throughSequence']) >= 1,
    'invalid_frame', 'Event availability sequence is invalid.');
}

export type ToolRouting = Wire.ToolCall | {
  serviceName: string; qualifiedName: string; expectedDefinitionHash: string; serviceNodeId?: string;
  arguments: unknown; operationId?: string; expectedCallerPrincipalId?: string;
};

/** Tool payloads are checked once by the selected domain schema when debug validation is enabled. */
export function validateToolRouting(value: unknown, discovery: boolean): asserts value is ToolRouting {
  const params = object(value, 'Tool call params must be an object.');
  const common = ['qualifiedName', 'arguments', 'expectedDefinitionHash', 'serviceNodeId', 'operationId', 'expectedCallerPrincipalId'];
  const allowed = discovery ? [...common, 'serviceName'] : [...common, 'hostId', 'serviceName', 'resourceRef'];
  requireThat(exactKeys(params, allowed) && text(params['qualifiedName']) && Object.hasOwn(params, 'arguments') && hash(params['expectedDefinitionHash']) &&
    (params['operationId'] === undefined || identifier(params['operationId'])) &&
    (params['expectedCallerPrincipalId'] === undefined || identifier(params['expectedCallerPrincipalId'])) &&
    optionalText(params['serviceNodeId']) && (params['serviceName'] === undefined || serviceName(params['serviceName'])) && optionalText(params['hostId']),
  'invalid_arguments', 'Tool routing fields are invalid.');
  if (discovery) requireThat(text(params['serviceName']), 'invalid_arguments', 'Discovery calls require a service name.');
  if (params['resourceRef'] !== undefined) {
    const reference = object(params['resourceRef'], 'Resource reference must be an object.');
    requireThat(resourceRef(reference), 'invalid_arguments', 'Resource routing fields are invalid.');
  }
}
