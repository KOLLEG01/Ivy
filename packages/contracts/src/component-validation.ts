import component from '../../../specs/schemas/component.schema.json' with { type: 'json' };
import host from '../../../specs/schemas/host.schema.json' with { type: 'json' };
import wire from '../../../specs/schemas/hive-wire.schema.json' with { type: 'json' };
import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema } from 'ajv';
import { requireThat } from './errors.js';

export const componentSchema = component as Record<string, unknown>;
const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [component, host, wire]) ajv.addSchema(schema as AnySchema);

export function validateComponent(value: unknown): void {
  const validator = ajv.getSchema(String(componentSchema['$id']));
  requireThat(validator && validator(value), 'invalid_arguments', 'Invalid component manifest.');
}
