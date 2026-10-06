import type { SQLInputValue } from 'node:sqlite';
import { canonical } from '../../../packages/contracts/src/canonical.js';
import { fail, requireThat } from '../../../packages/contracts/src/errors.js';
import { pointerParts, scalarTypes } from '../../../packages/contracts/src/schema.js';
import type { DataContract, ObjectQuery, Page, Predicate, QueryItem, Scalar } from '../../../packages/contracts/src/types.js';
import { HiveStore } from './store.js';
import type { Row } from './store.js';
import { Objects } from "./objects.js";

const metadata: Record<string, { column: string; types: string[] }> = {
  id: { column: 'o.id', types: ['string'] }, parentId: { column: 'o.parent_id', types: ['string', 'null'] },
  ownerObjectId: { column: 'o.owner_object_id', types: ['string', 'null'] },
  name: { column: 'o.name', types: ['string'] }, path: { column: 'o.path', types: ['string'] },
  position: { column: 'o.position', types: ['integer'] }, icon: { column: 'o.icon', types: ['string', 'null'] },
  contractKey: { column: 'o.contract_key', types: ['string'] }, revision: { column: 'r.revision', types: ['integer'] },
  contractVersion: { column: 'r.contract_version', types: ['string'] }, archivedAt: { column: 'o.archived_at', types: ['string', 'null'] },
  effectivelyArchived: { column: 'o.effective_archive', types: ['boolean'] },
  createdAt: { column: 'o.created_at', types: ['string'] }, updatedAt: { column: 'o.updated_at', types: ['string'] },
  // Navigation trees list one contract; attachments and other contracts do not make a branch expandable.
  hasContractChildren: { column: '(EXISTS(SELECT 1 FROM objects c WHERE c.parent_id=o.id AND c.contract_key=o.contract_key AND c.effective_archive=0))', types: ['boolean'] },
};
interface Expression { value: string; type: string; isNull: string }
interface SortBoundary { type: number; value: Scalar }
interface Boundary { sort: SortBoundary[]; id: string }

/** All input values, including JSON pointers, are bound parameters. Column names come from constants. */
class QueryBuilder {
  readonly bindings: Record<string, SQLInputValue> = {};
  private predicates = 0;
  constructor(readonly contracts: DataContract[] | null) {}
  bind(value: SQLInputValue): string {
    const name = 'p' + Object.keys(this.bindings).length;
    this.bindings[name] = value; return '$' + name;
  }
  field(field: string, operands?: Scalar[]): Expression {
    let expression: Expression;
    let definitions: Set<string>[];
    if (field.startsWith('object.')) {
      const selected = metadata[field.slice(7)];
      requireThat(selected, 'invalid_arguments', 'Unknown Object query field.');
      const rank = selected.types.includes('boolean') ? 1 : selected.types.includes('integer') ? 2 : 3;
      expression = { value: selected.column, type: `CASE WHEN ${selected.column} IS NULL THEN 9 ELSE ${rank} END`, isNull: `${selected.column} IS NULL` };
      definitions = [new Set(selected.types)];
    } else {
      requireThat(field.startsWith('data:/'), 'invalid_arguments', 'Query data fields require a JSON pointer.');
      const pointer = field.slice(5); pointerParts(pointer);
      const parameter = this.bind(pointer);
      const json = "CASE WHEN r.encoding='json' THEN r.content ELSE NULL END";
      expression = { value: `ivy_value(${json},${parameter})`, type: `ivy_type(${json},${parameter})`, isNull: `ivy_is_null(${json},${parameter})=1` };
      definitions = this.contracts?.map(contract => contract.jsonSchema === undefined ? new Set<string>() : scalarTypes(contract.jsonSchema, pointer)) ?? [];
    }
    for (const types of definitions) {
      requireThat(types.size, 'invalid_arguments', 'Field is not an admitted scalar in every selected contract version.');
      for (const operand of operands ?? []) {
        const type = operand === null ? 'null' : typeof operand;
        const admitted = types.has(type) || (type === 'number' && Number.isInteger(operand) && types.has('integer'));
        requireThat(admitted, 'invalid_arguments', 'Predicate operand is incompatible with the selected contract versions.');
      }
    }
    return expression;
  }
  predicate(predicate: Predicate, depth = 0): string {
    requireThat(++this.predicates <= 64 && depth <= 8, 'limit_exceeded', 'Query predicate exceeds its complexity limit.');
    if (predicate.op === 'and' || predicate.op === 'or') return '(' + predicate.args.map(child => this.predicate(child, depth + 1)).join(predicate.op === 'and' ? ' AND ' : ' OR ') + ')';
    if (predicate.op === 'not') return '(NOT ' + this.predicate(predicate.arg, depth + 1) + ')';
    // Narrow discriminants explicitly: the remaining forms all carry a field.
    if (!('field' in predicate)) return fail('invalid_arguments', 'Invalid query predicate.');
    const values = predicate.op === 'isNull' ? [] : predicate.op === 'in' ? predicate.value : [predicate.value];
    const field = this.field(predicate.field, values);
    if (predicate.op === 'isNull') return `(${field.isNull})`;
    if (predicate.op === 'contains') return `(${field.type}=3 AND COALESCE(instr(${field.value},${this.bind(predicate.value)})>0,0))`;
    const operators = { eq: '=', ne: '!=', gt: '>', gte: '>=', lt: '<', lte: '<=' };
    const operator = predicate.op === 'in' ? '=' : operators[predicate.op];
    return '(' + values.map(value => {
      if (value === null) return '0';
      const type = typeof value === 'boolean' ? 1 : typeof value === 'number' ? 2 : 3;
      const bound = this.bind(typeof value === 'boolean' ? Number(value) : value);
      // Type equality makes NULL comparisons definite false and distinguishes false from numeric 0.
      return `(${field.type}=${type} AND COALESCE(${field.value} COLLATE BINARY ${operator} ${bound},0))`;
    }).join(' OR ') + ')';
  }
}

export function queryObjects(
  store: HiveStore,
  params: ObjectQuery,
): Page<QueryItem> {
  const family = store.get(
    "SELECT media_type FROM contract_families WHERE key=?",
    params.contractKey,
  );
  requireThat(family, 'contract_not_found', 'Data Contract family not found.');
  requireThat(
    !params.includeContent ||
      /^application\/(?:json|[A-Za-z0-9!#$&^_.+-]+\+json)$/.test(
        String(family["media_type"]),
      ),
    'invalid_arguments',
    "Query content is available only for JSON contracts.",
  );
  const documents = params.includeContent ? new Objects(store) : null;
  const definitions = params.contractVersions ? params.contractVersions.map(version => store.contract(params.contractKey, version)) : null;
  const builder = new QueryBuilder(definitions);
  const projection = params.select ?? ['object.name', 'object.path'];
  const ordering = params.orderBy ?? [];
  const { cursor: _cursor, ...original } = params;
  const identity = { method: 'objects.query', ...original };
  const boundary = params.cursor ? store.readCursor(identity, params.cursor) as unknown as Boundary : null;
  const columns: string[] = ['o.id', 'r.revision', 'r.contract_version'];
  projection.forEach((field, i) => {
    const expr = builder.field(field);
    columns.push(`${expr.type} AS s${i}t`, `${expr.value} AS s${i}v`);
  });
  ordering.forEach((order, i) => {
    const expr = builder.field(order.field);
    columns.push(`${expr.type} AS k${i}t`, `${expr.value} AS k${i}v`);
  });
  const where = [`o.contract_key=${builder.bind(params.contractKey)}`, 'ivy_budget()'];
  if (!params.includeArchived) where.push('o.effective_archive=0');
  if (params.contractVersions) where.push('r.contract_version IN (' + params.contractVersions.map(version => builder.bind(version)).join(',') + ')');
  if (params.where) where.push(builder.predicate(params.where));
  const continueTerms: string[] = [], equalPrefix: string[] = [];
  const sorting: string[] = [];
  if (boundary) requireThat(Array.isArray(boundary.sort) && boundary.sort.length === ordering.length && typeof boundary.id === 'string', 'invalid_cursor', 'Invalid query boundary.');
  const term = (expression: string) => continueTerms.push('(' + [...equalPrefix, expression].join(' AND ') + ')');
  ordering.forEach((order, i) => {
    const direction = order.direction === 'desc' ? 'DESC' : 'ASC';
    sorting.push(`k${i}t ASC`, `k${i}v COLLATE BINARY ${direction}`);
    if (!boundary) return;
    const key = boundary.sort[i]!;
    const type = builder.bind(key.type), value = builder.bind(typeof key.value === 'boolean' ? Number(key.value) : key.value);
    term(`k${i}t > ${type}`);
    equalPrefix.push(`k${i}t = ${type}`);
    if (key.type !== 9) term(`k${i}v COLLATE BINARY ${direction === 'ASC' ? '>' : '<'} ${value}`);
    equalPrefix.push(`k${i}v IS ${value}`);
  });
  sorting.push('id ASC');
  if (boundary) term(`id > ${builder.bind(boundary.id)}`);
  const limit = params.limit ?? 50;
  const sql = `WITH matched AS (SELECT ${columns.join(',')} FROM objects o JOIN revisions r ON r.object_id=o.id AND r.revision=o.current_revision
    WHERE ${where.join(' AND ')}) SELECT * FROM matched ${boundary ? 'WHERE ' + continueTerms.join(' OR ') : ''} ORDER BY ${sorting.join(',')} LIMIT ${builder.bind(limit + 1)}`;
  // A null comparison is constant false; its admitted field need not leave an SQL parameter behind.
  const used = new Set((sql.match(/\$p\d+/g) ?? []).map(name => name.slice(1)));
  const bindings = Object.fromEntries(Object.entries(builder.bindings).filter(([name]) => used.has(name)));
  const rows = store.db.prepare(sql).all(bindings) as Row[];
  const items: QueryItem[] = [];
  let bytes = 128;
  for (const row of rows.slice(0, limit)) {
    const values: Record<string, Scalar> = Object.create(null) as Record<string, Scalar>;
    projection.forEach((field, i) => { values[field] = row[`s${i}t`] === 1 ? Boolean(row[`s${i}v`]) : row[`s${i}v`] as Scalar; });
    const item: QueryItem = { objectId: String(row['id']), revision: Number(row['revision']), contractVersion: String(row['contract_version']), values };
    if (documents)
      item.document = documents.read({
        objectId: item.objectId,
        revision: item.revision,
      });
    const size = Buffer.byteLength(canonical(item));
    if (bytes + size > 2 * 1024 * 1024 - 8192) {
      requireThat(items.length, 'result_too_large', 'A selected query row exceeds the response limit.');
      break;
    }
    bytes += size;
    items.push(item);
  }
  const last = rows[items.length - 1];
  const nextCursor = last && rows.length > items.length ? store.cursor(identity, {
    id: String(last['id']), sort: ordering.map((_, i) => ({ type: Number(last[`k${i}t`]), value: last[`k${i}v`] as Scalar })),
  }) : null;
  return { items, nextCursor };
}
