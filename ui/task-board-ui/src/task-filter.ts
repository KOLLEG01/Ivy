import { and, rootWhere } from './runtime';
import type { Wire } from './runtime';

/** Applied URL filters are independent of unfinished form edits and precede every page. */
export function taskFilter(root: string | null, query: URLSearchParams, defaultStatuses: string[] = []): Wire.QueryPredicate {
  const applied = Object.fromEntries(query), filters: Wire.QueryPredicate[] = [rootWhere(root)];
  if (applied.status) filters.push({ op: 'eq', field: 'data:/workflowState', value: applied.status });
  else if (defaultStatuses.length) filters.push({ op: 'in', field: 'data:/workflowState', value: defaultStatuses });
  if (applied.control) filters.push({ op: 'eq', field: 'data:/fields/control', value: applied.control });
  if (applied.host) filters.push({ op: 'eq', field: 'data:/fields/executionRequirement/hostId', value: applied.host });
  if (applied.category) filters.push({ op: 'eq', field: 'data:/fields/category', value: applied.category });
  if (applied.q) filters.push({ op: 'or', args: ['title', 'description'].map(field => ({ op: 'contains', field: 'data:/fields/' + field, value: applied.q! })) });
  return and(...filters);
}
