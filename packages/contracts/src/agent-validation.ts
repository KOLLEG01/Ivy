import agent from '../../../specs/schemas/agent.schema.json' with { type: 'json' };
import { wireSchema } from './wire-schema.js';
import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema } from 'ajv';
import { requireThat } from './errors.js';

export const agentSchema = agent as Record<string, unknown>;
const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [agent, wireSchema]) ajv.addSchema(schema as AnySchema);

export function validateAgent(name: string, value: unknown): void {
  const validator = ajv.getSchema(`${String(agentSchema['$id'])}#/$defs/${name}`);
  requireThat(validator && validator(value), 'invalid_arguments', 'Value does not match its native projection/AgentManager contract.');
}

/** Native JSONL has already been decoded and size-bounded by the transport. Keep this check structural. */
export function validateAgentFrame(value: unknown, kind: 'request-id' | 'reply' = 'reply'): void {
  if (kind === 'request-id') {
    requireThat((typeof value === 'string' && value.length > 0 && value.length <= 256) ||
      (typeof value === 'number' && Number.isSafeInteger(value)), 'native_invalid_frame', 'Native request ID is invalid.');
    return;
  }
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'native_invalid_frame', 'Native reply must be an object.');
  const reply = value as Record<string, unknown>;
  const result = Object.hasOwn(reply, 'result'), error = Object.hasOwn(reply, 'error');
  requireThat(result !== error, 'native_invalid_frame', 'Native reply requires exactly one result or error.');
  if (error) {
    const detail = reply['error'];
    requireThat(detail !== null && typeof detail === 'object' && !Array.isArray(detail), 'native_invalid_frame', 'Native error must be an object.');
    const item = detail as Record<string, unknown>;
    requireThat(Number.isSafeInteger(item['code']) && typeof item['message'] === 'string', 'native_invalid_frame', 'Native error envelope is incomplete.');
  }
}
