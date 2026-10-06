import { SchemaValidators } from '../../../../packages/sdk/src/node.js';
import type { Schema } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';

/** Compile each fixed local contract once; do not hash the entire graph on every IPC frame. */
export function phoneValidator(definitions: Record<string, unknown>) {
  const validators = new SchemaValidators();
  const prepared = new Map<string, Schema>();
  const freeze = (value: unknown): void => {
    if (value && typeof value === 'object' && !Object.isFrozen(value)) {
      for (const child of Object.values(value)) freeze(child);
      Object.freeze(value);
    }
  };
  const graph = structuredClone(definitions); freeze(graph);
  return (name: string, value: unknown, maximumBytes: number): void => {
    let schema = prepared.get(name);
    if (!schema) {
      requireThat(Object.hasOwn(graph, name), 'internal_error', 'Unknown Phone contract.');
      const selected: Record<string, unknown> = { [name]: graph[name] };
      const visit = (node: unknown): void => {
        if (Array.isArray(node)) {
          for (const child of node) visit(child);
          return;
        }
        if (!node || typeof node !== 'object') return;
        for (const [key, child] of Object.entries(node)) {
          if (key !== '$ref') {
            visit(child);
            continue;
          }
          requireThat(typeof child === 'string' && child.startsWith('#/$defs/'), 'internal_error', 'Phone contracts require local references.');
          const target = child.slice(8);
          requireThat(Object.hasOwn(graph, target), 'internal_error', 'Phone contract reference is missing.');
          if (!Object.hasOwn(selected, target)) {
            selected[target] = graph[target];
            visit(graph[target]);
          }
        }
      };
      visit(graph[name]);
      schema = Object.freeze({ $defs: Object.freeze(selected), $ref: '#/$defs/' + name }) as Schema;
      prepared.set(name, schema);
    }
    validators.validate(schema, value, maximumBytes);
  };
}
