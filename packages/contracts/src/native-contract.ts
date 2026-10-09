import { canonical, hashJson } from './canonical.js';
import { requireThat } from './errors.js';
import { SchemaValidators } from './schema.js';
import { validateAgent } from './agent-validation.js';
import { validateShared } from './core-validation.js';
import type { Agent, Wire } from './generated.js';
import type { Schema } from './types.js';
import { toolDefinitionHash } from './tool-definition.js';

/** Native calls whose protocol contract is observational and creates no receipt. */
const readOnlyNativeMethods = new Set([
  'account/rateLimits/read',
  'collaborationMode/list',
  'config/read',
  'fs/readFile',
  'model/list',
  'permissionProfile/list',
  'project/list',
  'server/diagnostics',
  'thread/items/list',
  'thread/list',
  'thread/loaded/list',
  'thread/goal/get',
  'thread/read',
  'thread/turns/list',
  'userVerification/status',
]);

export function isReadOnlyNativeMethod(method: string): boolean { return readOnlyNativeMethods.has(method); }

/** The provider's unchanged native definition. All consumers use this same projection. */
export function nativeToolDefinition(catalog: Agent.Catalog, method: Agent.Catalog['clientRequests'][number], catalogHash = hashJson(catalog)): Wire.ToolDefinition {
  const readOnly = isReadOnlyNativeMethod(method.method);
  return { namespace: 'codex', name: method.method, interfaceVersion: catalog.version,
    description: 'Installed public Codex ' + catalog.version + ' method ' + method.method + '. Supply its unchanged native arguments.' + (readOnly ? ' This read-only call does not require an operationId.' : ' Supply an outer stable operationId.'),
    inputSchema: method.inputSchema, outputSchema: method.outputSchema,
    ...(readOnly ? { annotations: { readOnlyHint: true, idempotentHint: true } } : {}),
    nativeMethod: method.method, nativeSchemaIdentity: catalogHash };
}

const freeze = (value: unknown): void => {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) { for (const child of Object.values(value)) freeze(child); Object.freeze(value); }
};

/** Exact selected native contract, shared by the SDK, AgentManager and native consumers. */
export class NativeContract {
  readonly catalog: Agent.Catalog;
  readonly catalogHash: string;
  private readonly methods = new Map<string, Wire.ToolDefinition>();
  private readonly definitionHashes = new Map<string, string>();
  private readonly validators = new SchemaValidators();
  private readonly schemas = new Map<string, Schema>();
  constructor(catalog: Agent.Catalog) {
    validateAgent('Catalog', catalog);
    this.catalog = structuredClone(catalog); freeze(this.catalog); this.catalogHash = hashJson(this.catalog);
    for (const method of this.catalog.clientRequests) {
      requireThat(!this.methods.has(method.method), 'native_catalog_invalid', 'The selected native catalog contains a duplicate method.');
      const definition = nativeToolDefinition(this.catalog, method, this.catalogHash); freeze(definition); this.methods.set(method.method, definition);
    }
  }
  definition(method: string): Wire.ToolDefinition {
    const value = this.methods.get(method);
    requireThat(value, 'native_method_unsupported', 'The method is absent from the exact selected native catalog.'); return value;
  }
  definitions(): ReadonlyMap<string, Wire.ToolDefinition> { return new Map(this.methods); }
  /** Retained definitions are deeply frozen; only their immutable local hashes are cached. */
  definitionHash(method: string): string {
    const existing = this.definitionHashes.get(method); if (existing) return existing;
    const value = toolDefinitionHash(this.definition(method)); this.definitionHashes.set(method, value); return value;
  }
  /** Check a discovery snapshot against the retained contract; never adopt a replacement. */
  verifyDefinition(method: string, definition: Wire.ToolDefinition, definitionHash: string): void {
    validateShared('ToolDefinition', definition);
    requireThat(definitionHash === toolDefinitionHash(definition) && definitionHash === this.definitionHash(method),
      'native_definition_changed', 'The selected provider definition differs from the retained native contract.');
  }
  /** Close only this adapter's outer arguments. Nested extension objects retain native semantics. */
  inputSchema(method: string, reserved: readonly string[] = []): Schema {
    const names = [...new Set(reserved)].sort(), key = canonical({ method, reserved: names });
    const cached = this.schemas.get(key); if (cached !== undefined) return cached;
    const input = structuredClone(this.definition(method).inputSchema) as Schema;
    let root = input;
    if (typeof input === 'object' && input['$ref'] !== undefined) {
      const defs = input['$defs'] as Record<string, Schema> | undefined, reference = input['$ref'];
      requireThat(typeof reference === 'string' && /^#\/\$defs\/[A-Za-z0-9]+$/.test(reference) && defs?.[reference.slice(8)] !== undefined,
        'native_catalog_invalid', 'Native arguments must use their exact finite local root definition.');
      root = defs![reference.slice(8)]!;
    }
    if (typeof root === 'object' && root['type'] === 'object' && root['properties'] && typeof root['properties'] === 'object') {
      const properties = root['properties'] as Record<string, unknown>;
      for (const name of names) delete properties[name];
      root['required'] = ((root['required'] ?? []) as string[]).filter(name => !names.includes(name));
      root['additionalProperties'] = false;
    } else {
      requireThat(names.length === 0, 'native_template_invalid', 'Only native object arguments can reserve workflow fields.');
      // Nullable/union request roots still contain outer argument objects. Close those
      // alternatives without changing nested native extension maps or union semantics.
      const visited = new Set<Schema>();
      const closeAlternative = (value: Schema): void => {
        if (typeof value !== 'object' || visited.has(value)) return;
        visited.add(value);
        if (value['$ref'] !== undefined) {
          const reference = value['$ref'], defs = typeof input === 'object' ? input['$defs'] as Record<string, Schema> | undefined : undefined;
          requireThat(typeof reference === 'string' && /^#\/\$defs\/[A-Za-z0-9]+$/.test(reference) && defs?.[reference.slice(8)] !== undefined,
            'native_catalog_invalid', 'Native argument alternatives must use finite local definitions.');
          closeAlternative(defs![reference.slice(8)]!);
        }
        if (value['type'] === 'object' && value['properties'] && typeof value['properties'] === 'object') value['additionalProperties'] = false;
        for (const key of ['anyOf', 'oneOf']) if (Array.isArray(value[key])) for (const child of value[key] as Schema[]) closeAlternative(child);
      };
      closeAlternative(root);
    }
    // Description text is immaterial to validation and can dominate admitted schema bytes.
    const strip = (value: unknown): void => {
      if (!value || typeof value !== 'object') return;
      for (const name of ['description', 'title', '$id', '$schema']) delete (value as Record<string, unknown>)[name];
      const node = value as Record<string, unknown>;
      for (const name of ['properties', '$defs']) if (node[name] && typeof node[name] === 'object') for (const child of Object.values(node[name])) strip(child);
      for (const name of ['anyOf', 'oneOf', 'allOf']) if (Array.isArray(node[name])) for (const child of node[name]) strip(child);
      for (const name of ['items', 'additionalProperties', 'if', 'then', 'else', 'not']) strip(node[name]);
    };
    strip(input); freeze(input); this.schemas.set(key, input); return input;
  }
  validateInput(method: string, params: unknown, maximumBytes = 1024 * 1024): void {
    this.validators.validate(this.inputSchema(method), params, maximumBytes);
  }
  validateTemplate(method: string, params: unknown, reserved: readonly string[], maximumBytes = 1024 * 1024): void {
    this.validators.validate(this.inputSchema(method, reserved), params, maximumBytes);
  }
  validateResult(method: string, result: unknown, maximumBytes = 1024 * 1024): void {
    this.validators.validate(this.definition(method).outputSchema, result, maximumBytes);
  }
}
