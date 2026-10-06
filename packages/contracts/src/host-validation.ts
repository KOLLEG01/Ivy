import host from '../../../specs/schemas/host.schema.json' with { type: 'json' };
import wire from '../../../specs/schemas/hive-wire.schema.json' with { type: 'json' };
import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema } from 'ajv';
import { managementFrameBytes } from './limits.js';
import { canonical } from './canonical.js';
import { requireThat } from './errors.js';

const schema = host as Record<string, unknown>;
const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
ajv.addSchema(schema as AnySchema);
ajv.addSchema(wire as AnySchema);
export function validateHost(name: string, value: unknown): void {
  canonical(value, 32 * 1024 * 1024);
  const validator = ajv.getSchema(`${schema.$id}#/$defs/${name}`);
  requireThat(validator && validator(value), 'invalid_arguments', 'Value does not match its host configuration/operation contract.');
}
export const hostSchema = schema;
export const hostManagementFrameBytes = managementFrameBytes;
