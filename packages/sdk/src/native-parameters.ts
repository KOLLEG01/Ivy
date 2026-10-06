import { canonical } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { NativeContract } from '../../contracts/src/native-contract.js';
import { SchemaValidators } from '../../contracts/src/schema.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import type { Agent, Wire } from '../../contracts/src/generated.js';
import type { RpcClient, RequestOptions } from './client.js';

const version = '1.0.0' as const, maximumBytes = 1048576;
const validators = new SchemaValidators();
const reservedNames = (reserved: readonly string[]) => [...new Set(reserved)].sort();
function header(contract: NativeContract, method: string, reserved: readonly string[]) {
  return { schemaVersion: 1 as const, nativeVersion: contract.catalog.version, nativeExecutableHash: contract.catalog.nativeExecutableHash,
    catalogHash: contract.catalogHash, method, reservedFields: reservedNames(reserved) };
}

function parametersSchema(contract: NativeContract, method: string, reserved: readonly string[] = []) {
  const input = contract.inputSchema(method, reserved), identity = header(contract, method, reserved);
  const { $defs, ...root } = typeof input === 'object' ? input : {}, params = typeof input === 'boolean' ? input : root;
  const jsonSchema = { type: 'object', properties: { ...Object.fromEntries(Object.entries(identity).map(([key, value]) => [key, { const: value }])), params },
    required: [...Object.keys(identity), 'params'], additionalProperties: false, ...($defs === undefined ? {} : { $defs }) };
  validators.compile(jsonSchema); return jsonSchema;
}

/** Bounded validated parameters live directly in the owning local workflow; no parameter Object is published. */
export async function saveNativeParameters(_client: RpcClient, contract: NativeContract, input: {
  method: string; params: Wire.Json; reserved?: readonly string[]; parentId: string | null; mutationId: string;
}, options?: RequestOptions): Promise<Agent.ParametersRef> {
  const { method } = input, reserved = reservedNames(input.reserved ?? []), params = structuredClone(input.params);
  const value: Agent.ParametersRef = { ...header(contract, method, reserved), params }; canonical(value, maximumBytes);
  contract.validateTemplate(method, params, reserved, maximumBytes);
  parametersSchema(contract, method, reserved); validateAgent('ParametersRef', value); return value;
}

export async function readNativeParameters(_client: RpcClient, contract: NativeContract, input: {
  method: string; reserved?: readonly string[]; parentId: string | null; ref: Agent.ParametersRef;
}, options?: RequestOptions): Promise<Wire.Json> {
  const { method } = input, value = structuredClone(input.ref), reserved = reservedNames(input.reserved ?? []), expected = header(contract, method, reserved);
  validateAgent('ParametersRef', value); canonical(value, maximumBytes); parametersSchema(contract, method, reserved);
  requireThat(value.nativeVersion === expected.nativeVersion && value.nativeExecutableHash === expected.nativeExecutableHash && value.catalogHash === expected.catalogHash &&
    value.method === method && canonical(value.reservedFields) === canonical(reserved), 'native_parameters_contract_mismatch', 'Native parameters belong to another selected catalog, method or template.');
  contract.validateTemplate(method, value.params, reserved, maximumBytes); return structuredClone(value.params);
}
