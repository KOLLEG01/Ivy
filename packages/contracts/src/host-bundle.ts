import { hostSchema } from './host-validation.js';
import { wireSchema } from './wire-schema.js';
import { requireThat } from './errors.js';

/** Produce a self-contained host-management schema without loading agent/chat/workflow schemas. */
export function bundledSchema(reference: string, document = String(hostSchema['$id'])): Record<string, unknown> {
  const sources = { Host: hostSchema, Wire: wireSchema };
  const documents = new Map(Object.entries(sources).map(([prefix, schema]) => [String(schema['$id']), { prefix, schema }]));
  const definitions: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
  const resolve = (ref: string, base: string): { value: Record<string, unknown>; document: string; name: string } => {
    const url = new URL(ref, base), documentId = url.origin + url.pathname, source = documents.get(documentId)?.schema;
    requireThat(source && /^#\/\$defs\/[A-Za-z0-9]+$/.test(url.hash), 'internal_error', 'Unknown host schema reference.');
    const name = url.hash.slice('#/$defs/'.length), value = (source['$defs'] as Record<string, Record<string, unknown>>)[name];
    requireThat(value, 'internal_error', 'Missing host schema definition.'); return { value, document: documentId, name };
  };
  const clone = (value: unknown, base: string): unknown => {
    if (Array.isArray(value)) return value.map(item => clone(item, base));
    if (value === null || typeof value !== 'object') return value;
    const result: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) {
      if (key === '$id' || key === '$schema') continue;
      if (key !== '$ref') { result[key] = clone(item, base); continue; }
      const resolved = resolve(String(item), base), name = documents.get(resolved.document)!.prefix + resolved.name;
      if (!Object.hasOwn(definitions, name)) { definitions[name] = true; definitions[name] = clone(resolved.value, resolved.document); }
      result[key] = '#/$defs/' + name;
    }
    return result;
  };
  let root = resolve(reference, document);
  while (typeof root.value['$ref'] === 'string') root = resolve(root.value['$ref'], root.document);
  return { ...clone(root.value, root.document) as Record<string, unknown>, $defs: definitions };
}
