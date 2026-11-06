import test, { mock } from 'node:test';
import assert from 'node:assert/strict';
import { Ajv2020 } from 'ajv/dist/2020.js';
import { definitionValidator } from '../packages/contracts/src/schema-definition.js';

function fixture() {
  const document = {
    $id: 'https://fixture.invalid/owned',
    $defs: {
      Root: { type: 'object', required: ['text'], properties: { text: { type: 'string' } }, anyOf: [{ type: 'object' }] },
      Allow: true,
      Deny: false,
      'sp/ace~é': { type: 'string' },
    },
  };
  const ajv = new Ajv2020({ strict: false, ownProperties: true });
  ajv.addSchema(document);
  return { ajv, document };
}

test('definition lookup refuses inherited fields before AJV resolution', () => {
  const { ajv, document } = fixture();
  const lookup = mock.method(ajv, 'getSchema');
  for (const name of ['constructor', 'toString', '__proto__', '%63onstructor', 'constructor/prototype',
    'Root/constructor', 'Root/type/constructor', 'Root/required/constructor', 'Root/required/length',
    'Root/required/00', 'Root/anyOf/toString', 'Root/anyOf/01']) {
    assert.equal(definitionValidator(ajv, document, name), undefined, name);
  }
  assert.equal(lookup.mock.callCount(), 0);
});

test('definition lookup preserves owned schemas, escaped names, array indices and false', () => {
  const { ajv, document } = fixture();
  const root = definitionValidator(ajv, document, '%52oot')!;
  assert.equal(root({ text: 'native' }), true);
  assert.equal(root({ text: 1 }), false);
  assert.equal(definitionValidator(ajv, document, 'sp~1ace~0%C3%A9')!('original'), true);
  assert.equal(definitionValidator(ajv, document, 'sp~1ace~0%C3%A9')!(1), false);
  assert.equal(definitionValidator(ajv, document, 'Root/anyOf/0')!({}), true);
  assert.equal(definitionValidator(ajv, document, 'Root/anyOf/0')!('x'), false);
  assert.equal(definitionValidator(ajv, document, 'Allow')!(null), true);
  assert.equal(definitionValidator(ajv, document, 'Deny')!(null), false);
});

test('definition lookup accepts genuinely owned prototype-like names', () => {
  const document = JSON.parse('{"$id":"https://fixture.invalid/names","$defs":{"constructor":{"type":"integer"},"__proto__":{"type":"string"},"toString":false}}');
  const ajv = new Ajv2020({ strict: false });
  ajv.addSchema(document);
  assert.equal(definitionValidator(ajv, document, 'constructor')!(42), true);
  assert.equal(definitionValidator(ajv, document, 'constructor')!('42'), false);
  assert.equal(definitionValidator(ajv, document, '__proto__')!('original'), true);
  assert.equal(definitionValidator(ajv, document, '__proto__')!(42), false);
  assert.equal(definitionValidator(ajv, document, 'toString')!(null), false);
});

test('definition lookup rejects owned non-schema values before compilation', () => {
  const { ajv, document } = fixture();
  const lookup = mock.method(ajv, 'getSchema');
  for (const name of ['Root/type', 'Root/required', 'Root/required/0']) {
    assert.throws(() => definitionValidator(ajv, document, name), {
      code: 'internal_error', message: 'Expected a JSON schema.', outcome: 'not_executed',
    });
  }
  assert.equal(lookup.mock.callCount(), 0);
});
