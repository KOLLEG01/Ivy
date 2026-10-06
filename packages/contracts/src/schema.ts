import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema, ValidateFunction } from 'ajv';
import { RE2JS } from 're2js';
import { canonical, hashJson } from './canonical.js';
import { fail, requireThat } from './errors.js';
import type { Schema } from './types.js';

const keywords = new Set([
  '$schema', '$id', '$defs', '$ref', 'title', 'description', 'default', 'examples', 'deprecated', 'readOnly', 'writeOnly', 'format',
  'type', 'properties', 'required', 'additionalProperties', 'minProperties', 'maxProperties',
  'items', 'minItems', 'maxItems', 'uniqueItems', 'enum', 'const', 'anyOf', 'oneOf', 'allOf',
  'if', 'then', 'else', 'not',
  'minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum', 'multipleOf', 'minLength', 'maxLength', 'pattern',
]);

export function pointerParts(pointer: string): string[] {
  requireThat(pointer.startsWith('/') && !/~(?:[^01]|$)/.test(pointer), 'invalid_arguments', 'Invalid JSON pointer.');
  return pointer.slice(1).split('/').map(part => part.replaceAll('~1', '/').replaceAll('~0', '~'));
}
function resolve(root: Schema, reference: string): Schema {
  requireThat(reference.startsWith('#/'), 'registry_invalid', 'Only local JSON pointer schema references are supported.');
  let value: unknown = root;
  for (const key of pointerParts(reference.slice(1))) {
    requireThat(value !== null && typeof value === 'object' && Object.hasOwn(value, key), 'registry_invalid', 'Unresolved schema reference.');
    value = (value as Record<string, unknown>)[key];
  }
  requireThat(typeof value === 'boolean' || (value !== null && typeof value === 'object' && !Array.isArray(value)), 'registry_invalid', 'Reference must address a schema.');
  return value as Schema;
}

/** Admit a finite schema graph. Recursive refs must consume an object property or array item. */
export function admitSchema(schema: Schema): void {
  canonical(schema, 256 * 1024);
  let visited = 0;
  const inspect = (node: Schema, depth: number, trail: Set<Schema>, consumption: number, refs: Map<Schema, number>): void => {
    requireThat(++visited <= 8192 && depth <= 64, 'limit_exceeded', 'Schema exceeds the validation complexity limit.');
    if (typeof node === 'boolean') return;
    requireThat(node !== null && typeof node === 'object' && !Array.isArray(node), 'registry_invalid', 'Expected a JSON schema.');
    for (const key of Object.keys(node)) requireThat(keywords.has(key), 'registry_invalid', `Unsupported schema keyword: ${key.slice(0, 80)}`);
    requireThat(!trail.has(node), 'registry_invalid', 'Schema has a non-consuming recursive reference.');
    const nextTrail = new Set(trail).add(node);
    if (node['$ref'] !== undefined) {
      requireThat(typeof node['$ref'] === 'string', 'registry_invalid', 'Invalid schema reference.');
      const target = resolve(schema, node['$ref']);
      const prior = refs.get(target);
      if (prior !== undefined) requireThat(consumption > prior, 'registry_invalid', 'Schema recursion must consume a value level.');
      else inspect(target, depth + 1, nextTrail, consumption, new Map(refs).set(target, consumption));
    }
    if (node['pattern'] !== undefined) {
      requireThat(typeof node['pattern'] === 'string' && node['pattern'].length <= 1024, 'registry_invalid', 'Schema pattern exceeds its limit.');
      try { RE2JS.compile(node['pattern']); } catch { fail('registry_invalid', 'Schema pattern is outside the supported RE2 syntax.'); }
    }
    for (const key of ['anyOf', 'oneOf', 'allOf']) if (node[key] !== undefined) {
      requireThat(Array.isArray(node[key]) && node[key].length > 0 && node[key].length <= 32, 'registry_invalid', 'Schema alternatives exceed their limit.');
      for (const child of node[key] as Schema[]) inspect(child, depth + 1, nextTrail, consumption, refs);
    }
    for (const key of ['if', 'then', 'else', 'not']) if (node[key] !== undefined) inspect(node[key] as Schema, depth + 1, nextTrail, consumption, refs);
    for (const key of ['properties', '$defs']) if (node[key] !== undefined) {
      const map = node[key];
      requireThat(map !== null && typeof map === 'object' && !Array.isArray(map), 'registry_invalid', 'Invalid schema property map.');
      requireThat(Object.keys(map).length <= 1024 && !Object.hasOwn(map, '__proto__'), 'registry_invalid', 'Unsupported schema property map.');
      for (const child of Object.values(map)) inspect(child as Schema, depth + 1, new Set(), consumption + (key === 'properties' ? 1 : 0), refs);
    }
    for (const key of ['items', 'additionalProperties']) if (node[key] !== undefined) inspect(node[key] as Schema, depth + 1, new Set(), consumption + 1, refs);
  };
  inspect(schema, 0, new Set(), 0, new Map([[schema, 0]]));
}

const re2Engine = Object.assign((pattern: string, _flags: string) => {
  const expression = RE2JS.compile(pattern);
  return { test: (input: string) => expression.matcher(input).find(), toString: () => pattern };
}, { code: 'ivyRE2' });

export class SchemaValidators {
  private readonly cache = new Map<string, ValidateFunction>();
  private readonly immutable = new WeakMap<object, ValidateFunction>();
  private anonymousCompiler: Ajv2020 | null = null;
  private compilations = 0;
  compile(schema: Schema): ValidateFunction {
    const frozen = typeof schema === 'object' && Object.isFrozen(schema);
    if (frozen) { const validator = this.immutable.get(schema); if (validator) return validator; }
    const identity = hashJson(schema);
    const existing = this.cache.get(identity);
    if (existing) { if (frozen) this.immutable.set(schema, existing); return existing; }
    admitSchema(schema);
    // Ajv's generated-code scope also retains dependencies after root unregistration.
    // Reset compiler and function cache together after a bounded batch, including failed compiles.
    if (this.compilations === 256) { this.cache.clear(); this.anonymousCompiler = null; this.compilations = 0; }
    this.compilations++;
    const options = { strict: false, allErrors: false, ownProperties: true, inlineRefs: false,
      validateFormats: false, loopRequired: 64, loopEnum: 64, code: { regExp: re2Engine } };
    const containsId = (value: unknown): boolean => value !== null && typeof value === 'object' &&
      (Object.hasOwn(value, '$id') || Object.values(value).some(containsId));
    // Explicit resource IDs keep an isolated compiler. Anonymous local-reference graphs can
    // reuse meta-schema setup without registering their roots or retaining an unbounded cache.
    const isolated = containsId(schema);
    const ajv = isolated ? new Ajv2020(options) : this.anonymousCompiler ??= new Ajv2020({ ...options, addUsedSchema: false });
    let validator: ValidateFunction;
    try { validator = ajv.compile(schema as AnySchema); } catch { fail('registry_invalid', 'Invalid or unsupported JSON schema.'); }
    finally { if (!isolated && typeof schema === 'object') ajv.removeSchema(schema as AnySchema); }
    this.cache.set(identity, validator);
    if (frozen) this.immutable.set(schema, validator);
    return validator;
  }
  validate(schema: Schema, value: unknown, maximumBytes = 1024 * 1024): void {
    canonical(value, maximumBytes);
    requireThat(this.compile(schema)(value), 'invalid_arguments', 'Content does not satisfy its exact contract.');
  }
}

/** Query admission is conservative: structured/ambiguous paths are never implicitly stringified. */
export function scalarTypes(schema: Schema, pointer: string): Set<string> {
  const parts = pointerParts(pointer);
  const walk = (node: Schema, index: number, depth: number): Set<string> => {
    requireThat(depth <= 64, 'invalid_arguments', 'Queryable schema path is too complex.');
    if (typeof node === 'boolean') return new Set();
    if (typeof node['$ref'] === 'string') return walk(resolve(schema, node['$ref']), index, depth + 1);
    for (const keyword of ['anyOf', 'oneOf']) if (Array.isArray(node[keyword])) {
      const sets = (node[keyword] as Schema[]).map(child => walk(child, index, depth + 1));
      return new Set(sets.flatMap(set => [...set]));
    }
    if (Array.isArray(node['allOf'])) {
      // Sibling properties/type constraints still apply when allOf adds state conditions. Omitting
      // them made otherwise declared scalar fields (for example Task.status) impossible to query.
      const { allOf, ...base } = node;
      const sets = [base, ...(allOf as Schema[])].map(child => walk(child, index, depth + 1)).filter(set => set.size);
      return sets.length ? sets.reduce((a, b) => {
        const intersection = new Set([...a].filter(type => b.has(type)));
        if ((a.has('number') && b.has('integer')) || (a.has('integer') && b.has('number'))) intersection.add('integer');
        return intersection;
      }) : new Set();
    }
    if (index === parts.length) {
      const literals = Object.hasOwn(node, 'const') ? [node['const']] : Array.isArray(node['enum']) ? node['enum'] : [];
      const implied = literals.map(value => value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value === 'number' && Number.isInteger(value) ? 'integer' : typeof value);
      const types = typeof node['type'] === 'string' ? [node['type']] : Array.isArray(node['type']) ? node['type'] : implied;
      if (types.some(t => t === 'object' || t === 'array')) return new Set();
      return new Set(types.filter((t): t is string => ['null', 'boolean', 'integer', 'number', 'string'].includes(String(t))));
    }
    const properties = node['properties'];
    if (properties && typeof properties === 'object' && Object.hasOwn(properties, parts[index]!)) {
      return walk((properties as Record<string, Schema>)[parts[index]!]!, index + 1, depth + 1);
    }
    if (node['type'] === 'array' && /^(0|[1-9]\d*)$/.test(parts[index]!) && node['items'] !== undefined) return walk(node['items'] as Schema, index + 1, depth + 1);
    return new Set();
  };
  return walk(schema, 0, 0);
}
