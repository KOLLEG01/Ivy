import secretary from '../../../specs/schemas/secretary.schema.json' with { type: 'json' };
import wire from '../../../specs/schemas/hive-wire.schema.json' with { type: 'json' };
import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema } from 'ajv';
import { canonical } from './canonical.js';
import { requireThat } from './errors.js';
import { managementFrameBytes } from './limits.js';

export const secretarySchema = secretary as Record<string, unknown>;
const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [secretary, wire]) ajv.addSchema(schema as AnySchema);

export function validateSecretary(name: string, value: unknown): void {
  canonical(value, managementFrameBytes);
  const validator = ajv.getSchema(`${String(secretarySchema['$id'])}#/$defs/${name}`);
  requireThat(validator && validator(value), 'invalid_arguments', 'Value does not match its Secretary contract.');
}
