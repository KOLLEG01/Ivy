import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema, ValidateFunction } from 'ajv';
import { requireThat } from './errors.js';
import { wireSchema } from './wire-schema.js';

// Services need the shared registry contract, but not Hive's operation and host
// schema trees. Keep this validator as the small wire-only boundary.
const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
ajv.addSchema(wireSchema as AnySchema);
const validators = new Map<string, ValidateFunction>();

export function validateShared(name: string, value: unknown): void {
  let validator = validators.get(name);
  if (!validator) {
    validator = ajv.getSchema(`${String(wireSchema['$id'])}#/$defs/${name}`);
    requireThat(validator, 'internal_error', 'Unknown shared contract.');
    validators.set(name, validator);
  }
  requireThat(validator(value), 'invalid_arguments', 'Value does not match its shared contract.');
}

export function matchesShared(name: string, value: unknown): boolean {
  let validator = validators.get(name);
  if (!validator) {
    validator = ajv.getSchema(`${String(wireSchema['$id'])}#/$defs/${name}`);
    requireThat(validator, 'internal_error', 'Unknown shared contract.');
    validators.set(name, validator);
  }
  return Boolean(validator(value));
}
