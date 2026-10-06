import { requireThat } from '../../contracts/src/errors.js';
import type { NativeContract } from '../../contracts/src/native-contract.js';
import type { Wire } from '../../contracts/src/generated.js';

type Node = Record<string, unknown>;
const record = (value: unknown): Node => {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'native_observation_shape_unsupported', 'Expected the selected native object shape.');
  return value as Node;
};
function resolve(root: Node, value: unknown): Node {
  let node = record(value); const seen = new Set<Node>();
  while (node['$ref'] !== undefined) {
    requireThat(!seen.has(node) && seen.size < 64 && typeof node['$ref'] === 'string' && /^#\/\$defs\/[A-Za-z0-9]+$/.test(node['$ref']),
      'native_observation_shape_unsupported', 'Native observation shape must resolve within its original finite schema.');
    seen.add(node); node = record(record(root['$defs'])[node['$ref'].slice(8)]);
  }
  return node;
}
function properties(root: Node, value: unknown): Node {
  const node = resolve(root, value);
  requireThat(node['type'] === 'object', 'native_observation_shape_unsupported', 'Expected the selected native object schema.');
  return record(node['properties']);
}
interface Layout { directInputFlag: boolean; items: 'direct' | 'turn-wrapper' }
const layouts = new WeakMap<NativeContract, Partial<Layout>>();
const layout = (contract: NativeContract) => { let value = layouts.get(contract); if (!value) { value = {}; layouts.set(contract, value); } return value; };

/** Capability follows the retained schema, including an optional flag whose absence denies input. */
export function nativeThreadState(contract: NativeContract, result: unknown): {
  thread: Record<string, Wire.Json>; canStartTurn: boolean; state: string;
} {
  const selected = layout(contract);
  if (selected.directInputFlag === undefined) {
    const schema = record(contract.definition('thread/read').outputSchema), thread = properties(schema, properties(schema, schema)['thread']);
    selected.directInputFlag = Object.hasOwn(thread, 'canAcceptDirectInput');
  }
  const thread = record(record(result)['thread']) as Record<string, Wire.Json>, state = record(thread['status'])['type'];
  requireThat(typeof state === 'string', 'native_observation_shape_unsupported', 'Native thread status requires its actual discriminator.');
  return { thread: structuredClone(thread), state, canStartTurn: ['idle', 'systemError'].includes(state) && (!selected.directInputFlag || thread['canAcceptDirectInput'] === true) };
}

/** Interpret a complete page; wrapped entries must each name the exact requested turn. */
export function nativeTurnItems(contract: NativeContract, result: unknown, turnId: string): Record<string, Wire.Json>[] {
  requireThat(typeof turnId === 'string' && turnId.length > 0, 'native_turn_mismatch', 'Native item interpretation requires the original turn ID.');
  const selected = layout(contract);
  if (!selected.items) {
    const schema = record(contract.definition('thread/items/list').outputSchema), data = resolve(schema, properties(schema, schema)['data']);
    requireThat(data['type'] === 'array', 'native_observation_shape_unsupported', 'Native item pages require the selected data array.');
    const item = resolve(schema, data['items']);
    if (item['type'] === 'object') {
      const fields = properties(schema, item), required = item['required'];
      requireThat(Object.hasOwn(fields, 'item') && Object.hasOwn(fields, 'turnId') && Array.isArray(required) && required.includes('item') && required.includes('turnId'),
        'native_observation_shape_unsupported', 'Unknown native item envelope.');
      selected.items = 'turn-wrapper';
    } else {
      const variants = item['oneOf'] ?? item['anyOf'];
      requireThat(Array.isArray(variants) && variants.length > 0 && variants.every(value => {
        const shape = resolve(schema, value), fields = properties(schema, shape), required = shape['required'];
        return Object.hasOwn(fields, 'type') && Object.hasOwn(fields, 'id') && Array.isArray(required) && required.includes('type') && required.includes('id');
      }), 'native_observation_shape_unsupported', 'Unknown native item union.');
      selected.items = 'direct';
    }
  }
  const data = record(result)['data']; requireThat(Array.isArray(data), 'native_observation_shape_unsupported', 'Native page lacks its complete data array.');
  return data.map(value => {
    const entry = record(value);
    if (selected.items === 'direct') return structuredClone(entry) as Record<string, Wire.Json>;
    requireThat(entry['turnId'] === turnId, 'native_turn_mismatch', 'Native item belongs to another turn.');
    return structuredClone(record(entry['item'])) as Record<string, Wire.Json>;
  });
}
