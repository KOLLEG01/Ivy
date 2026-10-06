import type { BoundTool, Wire } from '../../sdk/src/client.js';
import { enumStrings, list, record, schemaNode, supportsFields, text } from './native.js';
import type { JsonRecord, NativeModel } from './native.js';

export interface NativeMode { mode: string; name: string; effort: string | null }
function objectShape(value: unknown, root: unknown): JsonRecord {
  const node = schemaNode(value, root);
  if (node.type === 'object' || node.properties) return node;
  const objects = list(node.anyOf ?? node.oneOf).map(value => schemaNode(value, root)).filter(value => value.type === 'object' || value.properties);
  return objects.length === 1 ? objects[0]! : {};
}
function exactFields(shape: JsonRecord, fields: string[]): boolean {
  return fields.every(key => key in record(shape.properties)) && list(shape.required).every(key => typeof key === 'string' && fields.includes(key));
}
function primitive(value: unknown, schema: unknown, root: unknown, depth = 0): boolean {
  if (depth > 32) return false;
  const node = schemaNode(schema, root), choices = list(node.anyOf ?? node.oneOf);
  if (choices.length) { const matches = choices.filter(choice => primitive(value, choice, root, depth + 1)).length; return node.oneOf ? matches === 1 : matches > 0; }
  if (node.enum && !list(node.enum).includes(value as Wire.Json) || 'const' in node && node.const !== value) return false;
  const types = typeof node.type === 'string' ? [node.type] : list(node.type);
  if (value === null) return types.includes('null');
  return typeof value === 'string' && types.includes('string') && !node.pattern &&
    (typeof node.minLength !== 'number' || value.length >= node.minLength) && (typeof node.maxLength !== 'number' || value.length <= node.maxLength);
}
function shape(binding: BoundTool | undefined | null) {
  if (!binding || !supportsFields(binding, ['threadId', 'input', 'collaborationMode'])) return null;
  const root = binding.definition.inputSchema, outer = schemaNode(root, root), mode = objectShape(record(outer.properties).collaborationMode, root);
  const settings = objectShape(record(mode.properties).settings, root);
  return exactFields(mode, ['mode', 'settings']) && exactFields(settings, ['model', 'reasoning_effort', 'developer_instructions']) &&
    primitive(null, record(settings.properties).developer_instructions, root) ? { root, mode, settings } : null;
}
export function nativeModesFrom(binding: BoundTool | undefined | null, result: unknown): NativeMode[] {
  const fields = shape(binding); if (!fields) return [];
  const allowed = enumStrings(record(fields.mode.properties).mode, fields.root), seen = new Set<string>();
  const rows = list(record(result).data); if (rows.length > 32) return [];
  return rows.flatMap(value => {
    const row = record(value), mode = text(row.mode), name = text(row.name), effort = row.reasoning_effort ?? null;
    if (!mode || !name || !allowed.includes(mode) || seen.has(mode) || !(effort === null || typeof effort === 'string')) return [];
    seen.add(mode); return [{ mode, name, effort }];
  });
}
/** A mode change needs the explicitly selected advertised model; never infer the existing task model. */
export function nativeModePayload(binding: BoundTool | undefined | null, mode: NativeMode | undefined, model: NativeModel | undefined, effort: string): Wire.Json | null {
  const fields = shape(binding); if (!fields || !mode || !model?.model.trim() || !enumStrings(record(fields.mode.properties).mode, fields.root).includes(mode.mode)) return null;
  const reasoning = effort || mode.effort, props = record(fields.settings.properties);
  if (reasoning !== null && !model.efforts.includes(reasoning) || !primitive(model.model, props.model, fields.root) || !primitive(reasoning, props.reasoning_effort, fields.root)) return null;
  return { mode: mode.mode, settings: { model: model.model, reasoning_effort: reasoning, developer_instructions: null } };
}
