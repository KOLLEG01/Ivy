import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import path from 'node:path';
import ts from 'typescript';

const sha = value => 'sha256:' + createHash('sha256').update(value).digest('hex');
const schemaKeywords = new Set(['$schema', '$ref', 'definitions', 'title', 'description', 'default', 'type', 'properties', 'required',
  'additionalProperties', 'anyOf', 'oneOf', 'allOf', 'enum', 'items', 'format', 'minimum', 'maximum', 'minLength', 'maxLength']);
const annotations = new Set(['title', 'description', 'default']);
const propertyName = value => { assert.ok(ts.isIdentifier(value) || ts.isStringLiteral(value), 'Unsupported native property name.'); return value.text; };

/** Only checked exports and the corresponding tagged response mappings are inputs; no native method filter. */
export function generateProjection(input) {
  assert.equal(input.schemaVersion, 1); assert.equal(input.experimental, true);
  assert.ok(/^0\.\d+\.\d+$/.test(input.version));
  assert.ok(input.json && input.typescript && input.commonRust && input.v1Rust);
  const parsedJson = new Map(), parsedTs = new Map(), namedJson = new Map();
  for (const [name, value] of Object.entries(input.json)) {
    assert.ok(name.endsWith('.json') && !name.includes('..'));
    if (name.endsWith('.schemas.json')) continue; // Aggregate exports have namespace maps; standalone documents retain the same actual definitions.
    const schema = JSON.parse(value); parsedJson.set(name, schema);
    if (schema.title) namedJson.set(schema.title, { file: name, pointer: '#' });
  }
  for (const [file, schema] of parsedJson) for (const name of Object.keys(schema.definitions ?? {})) {
    if (!namedJson.has(name)) namedJson.set(name, { file, pointer: '#/definitions/' + name });
  }
  const tsFile = file => {
    if (parsedTs.has(file)) return parsedTs.get(file);
    assert.equal(typeof input.typescript[file], 'string', 'Missing actual TypeScript export: ' + file);
    const source = ts.createSourceFile(file, input.typescript[file], ts.ScriptTarget.Latest, true);
    assert.equal(source.parseDiagnostics.length, 0, 'Malformed actual TypeScript export: ' + file);
    const imports = new Map(), aliases = new Map();
    for (const node of source.statements) {
      if (ts.isImportDeclaration(node)) {
        assert.ok(ts.isStringLiteral(node.moduleSpecifier) && node.moduleSpecifier.text.startsWith('.'));
        assert.ok(node.importClause?.namedBindings && ts.isNamedImports(node.importClause.namedBindings));
        for (const item of node.importClause.namedBindings.elements) {
          const target = path.posix.normalize(path.posix.join(path.posix.dirname(file), node.moduleSpecifier.text + '.ts'));
          assert.ok(!target.startsWith('../')); assert.ok(!imports.has(item.name.text));
          imports.set(item.name.text, { file: target, name: item.propertyName?.text ?? item.name.text });
        }
      } else if (ts.isTypeAliasDeclaration(node)) {
        assert.ok(!node.typeParameters?.length && !aliases.has(node.name.text)); aliases.set(node.name.text, node.type);
      } else assert.fail('Unsupported statement in generated native TypeScript: ' + file);
    }
    const value = { source, imports, aliases }; parsedTs.set(file, value); return value;
  };
  const alias = (file, name) => {
    const module = tsFile(file), node = module.aliases.get(name);
    if (node) return { node, file, name };
    const imported = module.imports.get(name); assert.ok(imported, 'Unresolved native type: ' + file + '/' + name);
    return alias(imported.file, imported.name);
  };
  const rustMappings = macro => {
    const start = input.commonRust.indexOf(macro + '! {'); assert.ok(start >= 0);
    const tail = input.commonRust.slice(start + macro.length + 3), end = tail.search(/^}/m); assert.ok(end >= 0);
    const block = tail.slice(0, end), result = new Map();
    const entries = [...block.matchAll(/^ {4}(\w+)(?:\s*=>\s*"([^"]+)")?\s*\{([\s\S]*?)^ {4}\},/gm)];
    for (const entry of entries) {
      const method = entry[2] ?? entry[1][0].toLowerCase() + entry[1].slice(1);
      const response = /^\s*response:\s*([^,\r\n]+)/m.exec(entry[3])?.[1];
      assert.ok(response && !result.has(method), 'Missing/duplicate native response mapping: ' + method);
      result.set(method, response);
    }
    assert.ok(result.size); return result;
  };
  const tsMethods = name => {
    const file = name + '.ts', node = alias(file, name).node;
    const variants = ts.isUnionTypeNode(node) ? node.types : [node], result = new Map();
    for (const variant of variants) {
      assert.ok(ts.isTypeLiteralNode(variant)); const fields = new Map();
      for (const property of variant.members) { assert.ok(ts.isPropertySignature(property) && property.type); fields.set(propertyName(property.name), property); }
      const method = fields.get('method')?.type;
      assert.ok(method && ts.isLiteralTypeNode(method) && ts.isStringLiteral(method.literal));
      assert.ok(!result.has(method.literal.text));
      result.set(method.literal.text, { file, params: fields.get('params')?.type, paramsOptional: Boolean(fields.get('params')?.questionToken) });
    }
    return result;
  };
  const jsonMethods = name => {
    const document = parsedJson.get(name + '.json'); assert.ok(document);
    const variants = document.oneOf ?? document.anyOf; assert.ok(Array.isArray(variants));
    const result = new Map();
    for (const variant of variants) {
      const method = variant.properties?.method?.enum?.[0] ?? variant.properties?.method?.const;
      assert.equal(typeof method, 'string'); assert.ok(!result.has(method));
      result.set(method, { schema: variant.properties.params, required: (variant.required ?? []).includes('params') });
    }
    return result;
  };
  const fallbackTypes = new Set();
  const schemaFor = (native, file, tsType) => {
    const definitions = {}, identities = new Map();
    const define = (identity, build) => {
      if (identities.has(identity)) return { $ref: '#/$defs/' + identities.get(identity) };
      const key = 'd' + identities.size; identities.set(identity, key); definitions[key] = true;
      definitions[key] = build(); return { $ref: '#/$defs/' + key };
    };
    const at = (document, reference) => {
      assert.ok(reference === '#' || reference.startsWith('#/'));
      let value = document;
      for (const part of reference === '#' ? [] : reference.slice(2).split('/')) {
        const key = part.replaceAll('~1', '/').replaceAll('~0', '~');
        assert.ok(value && typeof value === 'object' && Object.hasOwn(value, key), 'Unresolved native JSON reference: ' + reference); value = value[key];
      }
      assert.ok(typeof value === 'boolean' || (value && typeof value === 'object' && !Array.isArray(value))); return value;
    };
    const jsonRef = (documentFile, reference) => define('json:' + documentFile + reference, () => normalize(at(parsedJson.get(documentFile), reference), documentFile));
    const normalize = (node, documentFile) => {
      if (typeof node === 'boolean') return node;
      assert.ok(node && typeof node === 'object' && !Array.isArray(node), 'Unsupported native JSON schema node.');
      for (const key of Object.keys(node)) assert.ok(schemaKeywords.has(key), 'Unsupported native JSON keyword: ' + key);
      if (node.$schema) assert.equal(node.$schema, 'http://json-schema.org/draft-07/schema#');
      if (node.$ref) {
        // Draft-07 ignores assertion siblings of $ref; fail instead of silently changing their meaning.
        assert.ok(Object.keys(node).every(key => key === '$ref' || annotations.has(key)), 'Native $ref has unsupported assertion siblings.');
        return { ...jsonRef(documentFile, node.$ref), ...Object.fromEntries(Object.entries(node).filter(([key]) => annotations.has(key))) };
      }
      const result = {};
      for (const [key, value] of Object.entries(node)) {
        if (key === '$schema' || key === 'definitions') continue;
        if (key === 'properties') result[key] = Object.fromEntries(Object.entries(value).map(([name, child]) => [name, normalize(child, documentFile)]));
        else if (['oneOf', 'anyOf', 'allOf'].includes(key)) { assert.ok(Array.isArray(value)); result[key] = value.map(child => normalize(child, documentFile)); }
        else if (['items', 'additionalProperties'].includes(key)) result[key] = normalize(value, documentFile);
        else result[key] = value;
      }
      return result;
    };
    const rustOptional = (name, properties) => {
      const body = new RegExp('pub struct ' + name + '\\s*\\{([\\s\\S]*?)^\\}', 'm').exec(input.v1Rust)?.[1];
      if (!body) return new Set();
      const result = new Set();
      for (const match of body.matchAll(/^\s*pub (\w+): Option</gm)) {
        const camel = match[1].replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
        const field = properties.has(match[1]) ? match[1] : camel;
        assert.ok(properties.has(field), 'Native Rust Option differs from generated TypeScript: ' + name + '.' + field); result.add(field);
      }
      return result;
    };
    const convert = (node, moduleFile, namedType) => {
      if (ts.isParenthesizedTypeNode(node)) return convert(node.type, moduleFile, namedType);
      if (ts.isUnionTypeNode(node)) return { anyOf: node.types.map(child => convert(child, moduleFile)) };
      if (ts.isLiteralTypeNode(node)) {
        if (node.literal.kind === ts.SyntaxKind.NullKeyword) return { type: 'null' };
        if (ts.isStringLiteral(node.literal)) return { const: node.literal.text };
        if (node.literal.kind === ts.SyntaxKind.TrueKeyword || node.literal.kind === ts.SyntaxKind.FalseKeyword) return { const: node.literal.kind === ts.SyntaxKind.TrueKeyword };
        assert.fail('Unsupported generated native literal.');
      }
      const primitive = { [ts.SyntaxKind.StringKeyword]: 'string', [ts.SyntaxKind.NumberKeyword]: 'number', [ts.SyntaxKind.BooleanKeyword]: 'boolean' }[node.kind];
      if (primitive) return { type: primitive };
      if (ts.isTypeReferenceNode(node)) {
        assert.ok(ts.isIdentifier(node.typeName) && !node.typeArguments?.length, 'Unsupported generated native generic.');
        const imported = alias(moduleFile, node.typeName.text), known = namedJson.get(imported.name);
        if (known) return jsonRef(known.file, known.pointer);
        return define('ts:' + imported.file + ':' + imported.name, () => { fallbackTypes.add(imported.name); return convert(imported.node, imported.file, imported.name); });
      }
      if (ts.isTypeLiteralNode(node)) {
        const properties = new Map();
        for (const member of node.members) {
          assert.ok(ts.isPropertySignature(member) && member.type, 'Unsupported native object member.');
          const name = propertyName(member.name); assert.ok(!properties.has(name)); properties.set(name, member);
        }
        const optional = rustOptional(namedType, properties), required = [], result = {};
        for (const [name, member] of properties) {
          if (optional.has(name)) assert.ok(ts.isUnionTypeNode(member.type) && member.type.types.some(value => ts.isLiteralTypeNode(value) && value.literal.kind === ts.SyntaxKind.NullKeyword), 'Rust Option lacks its actual nullable TS type.');
          if (!member.questionToken && !optional.has(name)) required.push(name);
          result[name] = convert(member.type, moduleFile);
        }
        return { type: 'object', properties: result, ...(required.length ? { required } : {}) };
      }
      assert.fail('Unsupported generated native TypeScript construct: ' + ts.SyntaxKind[node.kind]);
    };
    const root = native === undefined ? convert(tsType, file) : normalize(native, file);
    if (typeof root === 'boolean') return root;
    return { $schema: 'https://json-schema.org/draft/2020-12/schema', ...root, ...(Object.keys(definitions).length ? { $defs: definitions } : {}) };
  };
  const projection = { schemaVersion: 1, provider: 'codex', version: input.version, experimental: true, sourceHash: sha(JSON.stringify(input)),
    nativeExecutableHash: input.nativeExecutableHash, clientRequests: [], serverRequests: [], serverNotifications: [], clientNotifications: [], derivedLegacyTypes: [] };
  for (const [catalog, target, macro] of [['ClientRequest', 'clientRequests', 'client_request_definitions'], ['ServerRequest', 'serverRequests', 'server_request_definitions']]) {
    const mappings = rustMappings(macro), types = tsMethods(catalog), native = jsonMethods(catalog);
    assert.deepEqual([...mappings.keys()].sort(), [...types.keys()].sort(), 'Tagged method mappings differ from the installed TypeScript export.');
    for (const method of native.keys()) assert.ok(types.has(method), 'JSON method is missing from TypeScript: ' + method);
    for (const [method, type] of types) {
      const actual = native.get(method), responseType = mappings.get(method), segments = responseType.split('::'), name = segments.at(-1);
      assert.ok(/^\w+$/.test(name), 'Unsupported response type expression: ' + responseType);
      const candidates = [segments.length > 1 ? segments.slice(0, -1).join('/') + '/' + name + '.json' : name + '.json', name + '.json'];
      const responseFile = candidates.find(value => parsedJson.has(value));
      let output;
      if (responseFile) output = schemaFor(parsedJson.get(responseFile), responseFile);
      else {
        const tsPath = segments[0] === 'v2' ? 'v2/' + name + '.ts' : name + '.ts';
        const declaration = alias(tsPath, name); fallbackTypes.add(name);
        output = schemaFor(undefined, declaration.file, ts.factory.createTypeReferenceNode(name));
      }
      assert.ok(actual?.schema !== undefined || type.params, 'Missing complete native parameter definition: ' + method);
      const inputSchema = actual ? schemaFor(actual.schema, catalog + '.json') : schemaFor(undefined, type.file, type.params);
      projection[target].push({ method, paramsRequired: actual?.required ?? !type.paramsOptional, inputSchema, outputSchema: output,
        responseType, inputSource: actual ? 'native-json' : 'native-typescript-and-rust', outputSource: responseFile ? 'native-json' : 'native-typescript-and-rust' });
    }
  }
  for (const [catalog, target] of [['ServerNotification', 'serverNotifications'], ['ClientNotification', 'clientNotifications']]) {
    const types = tsMethods(catalog), native = jsonMethods(catalog);
    for (const method of native.keys()) assert.ok(types.has(method), 'JSON notification is missing from TypeScript: ' + method);
    for (const [method, type] of types) {
      const value = native.get(method), absent = value ? value.schema === undefined : !type.params;
      projection[target].push({ method, paramsRequired: value?.required ?? (!absent && !type.paramsOptional),
        inputSchema: absent ? { type: 'null' } : value ? schemaFor(value.schema, catalog + '.json') : schemaFor(undefined, type.file, type.params),
        paramsAbsent: absent, inputSource: value ? 'native-json' : 'native-typescript-and-json-types' });
    }
  }
  projection.derivedLegacyTypes = [...fallbackTypes].sort();
  return projection;
}
