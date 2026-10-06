import { hashJson } from './canonical.js';
import type { Wire } from './generated.js';

/** Discovery text only affects navigation; callable identity covers the executable contract. */
export function toolDefinitionHash(definition: Wire.ToolDefinition): string {
  const { discovery: _discovery, ...callable } = definition;
  return hashJson(callable);
}
