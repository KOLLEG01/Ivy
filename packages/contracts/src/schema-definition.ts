import type { Ajv2020 } from 'ajv/dist/2020.js';
import type { ValidateFunction } from 'ajv';
import { requireThat } from './errors.js';

/** Resolve repository definitions through owned JSON fields before AJV compilation. */
export function definitionValidator(ajv: Ajv2020, document: Record<string, unknown>, name: string): ValidateFunction | undefined {
  const reference = `${String(document['$id'])}#/$defs/${name}`;
  const fragment = ajv.opts.uriResolver.parse(reference).fragment;
  if (!fragment?.startsWith('/')) return undefined;
  let selected: unknown = document;
  for (const part of fragment.slice(1).split('/')) {
    const key = decodeURIComponent(part).replaceAll('~1', '/').replaceAll('~0', '~');
    if (selected === null || typeof selected !== 'object' ||
      (Array.isArray(selected) && !/^(0|[1-9]\d*)$/.test(key)) || !Object.hasOwn(selected, key)) return undefined;
    selected = (selected as Record<string, unknown>)[key];
  }
  requireThat(typeof selected === 'boolean' || (selected !== null && typeof selected === 'object' && !Array.isArray(selected)),
    'internal_error', 'Expected a JSON schema.');
  return ajv.getSchema(reference);
}
