import agent from '../../../specs/schemas/agent.schema.json' with { type: 'json' };
import { wireSchema } from './wire-schema.js';
import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema } from 'ajv';
import { requireThat } from './errors.js';
import { definitionValidator } from './schema-definition.js';

export const agentSchema = agent as Record<string, unknown>;
const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [agent, wireSchema]) ajv.addSchema(schema as AnySchema);

export function validateAgent(name: string, value: unknown): void {
  const validator = definitionValidator(ajv, agentSchema, name);
  requireThat(validator && validator(value), 'invalid_arguments', 'Value does not match its native projection/AgentManager contract.');
}

export { validateAgentFrame } from './agent-frame.js';
