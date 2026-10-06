import test from 'node:test';
import assert from 'node:assert/strict';
import { SchemaValidators, scalarTypes } from '../packages/contracts/src/schema.js';
import { bundleLocalSchema } from '../packages/contracts/src/local-bundle.js';
import { hashJson } from '../packages/contracts/src/canonical.js';

test('local bundles ignore unrelated definitions but retain transitive, recursive and escaped references', () => {
  const document = { $defs: { Root: { type: 'object', properties: { value: { $ref: '#/$defs/a~1b' }, next: { $ref: '#/$defs/Root' } }, additionalProperties: false }, 'a/b': { type: 'integer' }, Settings: { type: 'string' } } };
  const first = bundleLocalSchema(document, 'Root'), changed = structuredClone(document); changed.$defs.Settings.type = 'number';
  assert.equal(hashJson(first), hashJson(bundleLocalSchema(changed, 'Root')));
  assert.deepEqual(Object.keys(first['$defs'] as object).sort(), ['Root', 'a/b']);
  const validate = new SchemaValidators().compile(first); assert.equal(validate({ value: 7, next: { value: 8 } }), true); assert.equal(validate({ value: 7.5 }), false);
  changed.$defs['a/b'].type = 'string'; assert.notEqual(hashJson(first), hashJson(bundleLocalSchema(changed, 'Root')));
  assert.throws(() => bundleLocalSchema(document, 'Missing'), { code: 'registry_invalid' });
  assert.throws(() => bundleLocalSchema({ $defs: { Root: { $ref: 'https://example.test/schema' } } }, 'Root'), { code: 'registry_invalid' });
  assert.equal((document.$defs.Root.properties.value).$ref, '#/$defs/a~1b', 'Bundling must not mutate the source.');
  const literal = bundleLocalSchema({ $defs: { Root: { properties: { default: { $ref: '#/$defs/Value' } }, const: { $ref: '#/$defs/NotASchemaReference' } }, Value: { type: 'string' } } }, 'Root');
  assert.deepEqual(Object.keys(literal['$defs'] as object).sort(), ['Root', 'Value']);
});

test('scalar intersection respects integer subtyping through local references and nullable compositions', () => {
  for (const pair of [['number', 'integer'], ['integer', 'number']]) {
    const schema = { $defs: { Numeric: { type: [pair[0]!, 'null'] } }, type: 'object', properties: { value: { allOf: [{ $ref: '#/$defs/Numeric' }, { type: [pair[1]!, 'null'] }] } } };
    assert.deepEqual([...scalarTypes(schema, '/value')].sort(), ['integer', 'null']);
    const validate = new SchemaValidators().compile(schema);
    assert.equal(validate({ value: 7 }), true); assert.equal(validate({ value: null }), true); assert.equal(validate({ value: 7.5 }), false);
  }
  assert.deepEqual([...scalarTypes({ properties: { value: { allOf: [{ type: 'string' }, { type: 'integer' }] } } }, '/value')], []);
});

test('anonymous compiler reuse preserves incompatible local definitions, recursion and cached functions after eviction', () => {
  const validators = new SchemaValidators();
  const string = { $ref: '#/$defs/value', $defs: { value: { type: 'string' } } };
  const integer = { $ref: '#/$defs/value', $defs: { value: { type: 'integer' } } };
  const first = validators.compile(string), second = validators.compile(integer);
  assert.equal(first('saved'), true); assert.equal(first(1), false);
  assert.equal(second(1), true); assert.equal(second('saved'), false);
  const recursive = validators.compile({ $ref: '#/$defs/node', $defs: { node: { type: 'object', properties: { value: { type: 'integer' }, next: { $ref: '#/$defs/node' } }, required: ['value'], additionalProperties: false } } });
  assert.equal(recursive({ value: 1, next: { value: 2 } }), true); assert.equal(recursive({ value: 1, next: { value: 'wrong' } }), false);
  for (let index = 0; index < 260; index++) validators.compile({ const: index });
  assert.equal(first('still correct'), true); assert.equal(first(3), false);
  assert.equal(second(3), true); assert.equal(second('still wrong'), false);
  const rebuilt = validators.compile(string); assert.notEqual(rebuilt, first); assert.equal(rebuilt(1), false); assert.equal(rebuilt('saved'), true);
});

test('explicit schema IDs remain independent and a failed anonymous compile cannot poison later admission', () => {
  const validators = new SchemaValidators(), id = 'https://fixture.invalid/schema';
  const first = validators.compile({ $id: id, $ref: '#/$defs/value', $defs: { value: { type: 'string' } } });
  const second = validators.compile({ $id: id, $ref: '#/$defs/value', $defs: { value: { type: 'integer' } } });
  assert.equal(first('saved'), true); assert.equal(first(1), false); assert.equal(second(1), true); assert.equal(second('saved'), false);
  assert.throws(() => validators.compile({ type: 'invalid-type' }), { code: 'registry_invalid' });
  assert.throws(() => validators.compile({ $ref: '#/$defs/value', $defs: { value: { $ref: '#/$defs/value' } } }));
  assert.throws(() => validators.compile({ $ref: 'https://fixture.invalid/external' }));
  const after = validators.compile({ $ref: '#/$defs/value', $defs: { value: { type: 'string', pattern: '^safe+$' } } });
  assert.equal(after('safe'), true); assert.equal(after('unsafe'), false);
  assert.equal(validators.compile(true)(null), true); assert.equal(validators.compile(false)(null), false);
});
