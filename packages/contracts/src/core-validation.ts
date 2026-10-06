import { managementFrameBytes } from './limits.js';
import { wireSchema } from './wire-schema.js';
import operation from '../../../specs/schemas/hive-operations.schema.json' with { type: 'json' };
import transport from '../../../specs/schemas/hive-transport.schema.json' with { type: 'json' };
import host from '../../../specs/schemas/host.schema.json' with { type: 'json' };
import catalog from '../../../specs/schemas/operations.json' with { type: 'json' };
import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema, ValidateFunction } from 'ajv';
import { fail, requireThat } from './errors.js';
import type { Operation } from './generated.js';

export { wireSchema };
export const operationSchema = operation as Record<string, unknown>;
export const transportSchema = transport as Record<string, unknown>;
export interface OperationDefinition { input: string; output: string; access: 'client' | 'service'; mutation: boolean; discoverable?: boolean }
export const operations = catalog.operations as Record<string, OperationDefinition>;

const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [wireSchema, operationSchema, transportSchema, host]) ajv.addSchema(schema as AnySchema);
const get = (ref: string): ValidateFunction => {
  const validator = ajv.getSchema(ref);
  if (!validator) throw new Error(`Unknown internal schema: ${ref}`);
  return validator;
};
const inputValidators = new Map<string, ValidateFunction>();
const outputValidators = new Map<string, ValidateFunction>();
function operationValidator(method: string, output: boolean): ValidateFunction | undefined {
  if (!Object.hasOwn(operations, method)) return undefined;
  const validators = output ? outputValidators : inputValidators;
  let validator = validators.get(method);
  if (!validator) {
    const definition = operations[method]!;
    validator = get(String(operationSchema['$id']) + (output ? definition.output : definition.input));
    validators.set(method, validator);
  }
  return validator;
}

export interface RpcRequest { jsonrpc: '2.0'; id: string | number; method: string; params: Record<string, unknown> }
export function validateRequest(value: unknown): asserts value is RpcRequest {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'invalid_frame', 'Expected one JSON-RPC request envelope.');
  const frame = value as Record<string, unknown>;
  requireThat(frame['jsonrpc'] === '2.0' && Object.keys(frame).every(key => ['jsonrpc', 'id', 'method', 'params'].includes(key)),
    'invalid_frame', 'Expected one exact JSON-RPC 2.0 request envelope.');
  const id = frame['id'];
  requireThat((typeof id === 'string' && id.length > 0 && id.length <= 256) ||
    (typeof id === 'number' && Number.isSafeInteger(id) && id >= 0), 'invalid_frame', 'Request ID is outside its bounded JSON-RPC domain.');
  requireThat(typeof frame['method'] === 'string' && frame['method'].length > 0 && frame['method'].length <= 192,
    'invalid_frame', 'Request method is outside its bounded JSON-RPC domain.');
  const params = frame['params'];
  requireThat(params !== null && typeof params === 'object' && !Array.isArray(params), 'invalid_frame', 'Request params must be an object.');
}
/** Validate only the provider response envelope; the selected tool contract is checked separately in debug mode. */
export function validateResponseFrame(value: unknown): asserts value is { id: string; result?: unknown; error?: Record<string, unknown> } {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'invalid_frame', 'Expected one provider response envelope.');
  const frame = value as Record<string, unknown>;
  requireThat(frame['jsonrpc'] === '2.0' && typeof frame['id'] === 'string' && frame['id'].length > 0 && frame['id'].length <= 256,
    'invalid_frame', 'Provider response correlation is invalid.');
  const hasResult = Object.hasOwn(frame, 'result'), hasError = Object.hasOwn(frame, 'error');
  requireThat(hasResult !== hasError, 'invalid_frame', 'Provider response requires exactly one result or error.');
  requireThat(Object.keys(frame).every(key => (hasResult ? ['jsonrpc', 'id', 'result'] : ['jsonrpc', 'id', 'error']).includes(key)),
    'invalid_frame', 'Provider response envelope contains unexpected fields.');
  if (hasError) {
    const error = frame['error'];
    requireThat(error !== null && typeof error === 'object' && !Array.isArray(error), 'invalid_frame', 'Provider error must be an object.');
    const errorObject = error as Record<string, unknown>, data = errorObject['data'];
    requireThat(Object.keys(errorObject).every(key => ['code', 'message', 'data'].includes(key)) && Number.isSafeInteger(errorObject['code']) &&
      typeof errorObject['message'] === 'string' && errorObject['message'].length > 0 && errorObject['message'].length <= 2048 &&
      data !== null && typeof data === 'object' && !Array.isArray(data), 'invalid_frame', 'Provider error envelope is incomplete.');
    const dataObject = data as Record<string, unknown>;
    requireThat(Object.keys(dataObject).every(key => ['code', 'outcome', 'details'].includes(key)) && typeof dataObject['code'] === 'string' &&
      dataObject['code'].length > 0 && dataObject['code'].length <= 128 && ['not_executed', 'unknown', 'completed'].includes(String(dataObject['outcome'])),
      'invalid_frame', 'Provider error outcome is invalid.');
  }
}
export function validateInput(method: string, value: unknown): void {
  const validator = operationValidator(method, false);
  requireThat(validator, 'not_found', 'Unknown Hive operation.');
  requireThat(validator(value), 'invalid_arguments', 'Arguments do not match the operation contract.');
}
export function validateOutput(method: string, value: unknown): void {
  const validator = operationValidator(method, true);
  requireThat(validator && validator(value), 'internal_error', 'Result does not match the operation contract.');
}
export function validateUiDefinition(value: unknown): void {
  requireThat(get(`${String(operationSchema['$id'])}#/$defs/UiDefinition`)(value), 'invalid_arguments', 'Invalid static ui definition.');
}
export function isUiRelease(value: unknown): value is Operation.UiRelease {
  return Boolean(get(`${String(operationSchema['$id'])}#/$defs/UiRelease`)(value));
}
export function isUiMetadata(value: unknown): value is Operation.UiMetadata {
  return Boolean(get(`${String(operationSchema['$id'])}#/$defs/UiMetadata`)(value));
}
export function validateShared(name: string, value: unknown): void {
  requireThat(get(`${String(wireSchema['$id'])}#/$defs/${name}`)(value), 'invalid_arguments', 'Value does not match its shared contract.');
}
export function validateTransport(name: string, value: unknown): void {
  requireThat(get(`${String(transportSchema['$id'])}#/$defs/${name}`)(value), 'invalid_frame', 'Invalid provider transport frame.');
}

/** Used by the live Hive path; the expensive service-domain schema trees stay out of this module. */
export const coreManagementFrameBytes = managementFrameBytes;
