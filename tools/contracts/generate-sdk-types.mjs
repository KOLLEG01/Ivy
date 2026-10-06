import { readFileSync, writeFileSync } from 'node:fs';

// Structural TS types are derived from the normative schemas. Value/state constraints stay in
// canonical runtime validation; TypeScript cannot express numeric bounds, regex or exclusive keys.
const documents = ['hive-wire', 'hive-operations', 'hive-transport', 'host', 'agent', 'task-board', 'automation', 'chat', 'secretary'].map((file, index) => ({
  schema: JSON.parse(readFileSync(`specs/schemas/${file}.schema.json`, 'utf8')),
  name: ['Wire', 'Operation', 'Transport', 'Host', 'Agent', 'TaskBoard', 'Automation', 'Chat', 'Secretary'][index],
}));
function reference(ref, doc) {
  const url = new URL(ref, doc.schema.$id);
  const target = documents.find(candidate => candidate.schema.$id === url.origin + url.pathname);
  const key = url.hash.slice('#/$defs/'.length);
  if (!target || !url.hash.startsWith('#/$defs/') || !target.schema.$defs[key] || !/^[A-Za-z][A-Za-z0-9]*$/.test(key)) throw new Error(`Unresolved TS reference ${ref}`);
  return `${target.name}.${key}`;
}
function convert(schema, doc) {
  if (schema === true) return 'unknown';
  if (schema === false) return 'never';
  if (schema.$ref) return reference(schema.$ref, doc);
  if ('const' in schema) return JSON.stringify(schema.const);
  if (schema.enum) return schema.enum.map(value => JSON.stringify(value)).join(' | ');
  if (Array.isArray(schema.type)) return schema.type.map(type => `(${convert({ ...schema, type }, doc)})`).join(' | ');
  const variants = schema.oneOf ?? schema.anyOf;
  if (variants) {
    const { oneOf, anyOf, ...base } = schema;
    return variants.map(variant => `(${convert({ ...base, ...variant, ...(base.properties ? { properties: { ...base.properties, ...variant.properties } } : {}), required: [...new Set([...(base.required ?? []), ...(variant.required ?? [])])] }, doc)})`).join(' | ');
  }
  if (schema.allOf) {
    const { allOf, ...base } = schema;
    return [base, ...allOf].map(value => `(${convert(value, doc)})`).join(' & ');
  }
  switch (schema.type) {
    case 'null': case 'boolean': case 'string': return schema.type;
    case 'integer': case 'number': return 'number';
    case 'array': return `Array<${convert(schema.items ?? true, doc)}>`;
    case 'object': {
      const properties = Object.entries(schema.properties ?? {}).map(([key, value]) => `${JSON.stringify(key)}${schema.required?.includes(key) ? '' : '?'}: ${convert(value, doc)};`);
      if (schema.additionalProperties !== false) properties.push(`[key: string]: ${schema.additionalProperties && schema.additionalProperties !== true ? convert(schema.additionalProperties, doc) : 'unknown'};`);
      return `{ ${properties.join(' ')} }`;
    }
    case undefined: return 'unknown';
    default: throw new Error(`Unsupported TS type ${schema.type}`);
  }
}
const operations = JSON.parse(readFileSync('specs/schemas/operations.json', 'utf8')).operations;
let text = '// Generated from specs/schemas; run npm run contracts:generate. Do not edit.\n';
for (const doc of documents) {
  text += `export namespace ${doc.name} {\n`;
  for (const [name, definition] of Object.entries(doc.schema.$defs)) text += `  export type ${name} = ${convert(definition, doc)};\n`;
  text += '}\n';
}
text += 'export interface OperationMap {\n';
for (const [method, definition] of Object.entries(operations)) text += `  ${JSON.stringify(method)}: { params: ${reference(definition.input, documents[1])}; result: ${reference(definition.output, documents[1])} };\n`;
text += '}\nexport type OperationName = keyof OperationMap;\nexport type Params<M extends OperationName> = OperationMap[M]["params"];\nexport type Result<M extends OperationName> = OperationMap[M]["result"];\n';
const path = 'packages/contracts/src/generated.ts';
if (process.argv.includes('--check')) {
  if (readFileSync(path, 'utf8') !== text) throw new Error(`Stale generated contract: ${path}`);
} else writeFileSync(path, text);
