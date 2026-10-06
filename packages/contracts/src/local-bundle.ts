import { requireThat } from './errors.js';

/** Bundle one local definition and its reachable definitions without unrelated contract inputs. */
export function bundleLocalSchema(document: { $schema?: string; $defs: Record<string, unknown> }, name: string): Record<string, unknown> {
  const definitions: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
  const visit = (value: unknown): void => {
    if (Array.isArray(value)) { for (const child of value) visit(child); return; }
    if (!value || typeof value !== 'object') return;
    for (const [key, child] of Object.entries(value)) {
      if (key === '$ref') {
        requireThat(typeof child === 'string' && /^#\/\$defs\/[^/]+(?:\/.*)?$/.test(child), 'registry_invalid', 'Only local definition references may be bundled.');
        include(child.split('/')[2]!.replaceAll('~1', '/').replaceAll('~0', '~'));
      } else if (['properties', '$defs'].includes(key) && child && typeof child === 'object') {
        for (const schema of Object.values(child)) visit(schema);
      } else if (['items', 'additionalProperties', 'allOf', 'anyOf', 'oneOf', 'if', 'then', 'else', 'not'].includes(key)) visit(child);
    }
  };
  const include = (key: string): void => {
    if (Object.hasOwn(definitions, key)) return;
    requireThat(Object.hasOwn(document.$defs, key), 'registry_invalid', 'Missing bundled schema definition.');
    definitions[key] = structuredClone(document.$defs[key]);
    visit(definitions[key]);
  };
  include(name);
  return { ...(document.$schema ? { $schema: document.$schema } : {}), $ref: '#/$defs/' + name.replaceAll('~', '~0').replaceAll('/', '~1'), $defs: definitions };
}
