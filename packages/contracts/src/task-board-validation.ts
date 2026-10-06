import agent from '../../../specs/schemas/agent.schema.json' with { type: 'json' };
import chat from '../../../specs/schemas/chat.schema.json' with { type: 'json' };
import taskBoard from '../../../specs/schemas/task-board.schema.json' with { type: 'json' };
import wire from '../../../specs/schemas/hive-wire.schema.json' with { type: 'json' };
import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema } from 'ajv';
import { canonical } from './canonical.js';
import { requireThat } from './errors.js';
import { managementFrameBytes } from './limits.js';

export const taskBoardSchema = taskBoard as Record<string, unknown>;
const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [taskBoard, chat, agent, wire]) ajv.addSchema(schema as AnySchema);

export function validateTaskBoard(name: string, value: unknown): void {
  canonical(value, managementFrameBytes);
  const validator = ajv.getSchema(`${String(taskBoardSchema['$id'])}#/$defs/${name}`);
  requireThat(validator && validator(value), 'invalid_arguments', 'Value does not match its TaskBoard workflow contract.');
}
