import { Ajv2020 } from 'ajv/dist/2020.js';
import standaloneCode from 'ajv/dist/standalone/index.js';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

/** Fixed contracts compile at build time; published pages keep script-src 'self'. */
export function writeBrowserAgentValidation(project, directory) {
  const schema = name => JSON.parse(readFileSync(resolve(project, 'specs/schemas', name + '.schema.json'), 'utf8'));
  const agent = schema('agent');
  const ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false,
    ownProperties: true, inlineRefs: false, code: { source: true } });
  for (const document of [schema('hive-wire'), agent]) ajv.addSchema(document);
  const definitions = Object.fromEntries(Object.keys(agent.$defs).map(name => [name, agent.$id + '#/$defs/' + name]));
  mkdirSync(directory, { recursive: true });
  const validators = resolve(directory, 'validators.cjs');
  writeFileSync(validators, standaloneCode(ajv, definitions));
  const entry = resolve(directory, 'agent-validation.mjs');
  const source = file => JSON.stringify(resolve(project, 'packages/contracts/src', file).replaceAll('\\', '/'));
  writeFileSync(entry, `import validators from './validators.cjs';
import agent from ${JSON.stringify(resolve(project, 'specs/schemas/agent.schema.json').replaceAll('\\', '/'))};
import { requireThat } from ${source('errors.ts')};
export { validateAgentFrame } from ${source('agent-frame.ts')};
export const agentSchema = agent;
export function validateAgent(name, value) {
  const validator = Object.hasOwn(validators, name) ? validators[name] : undefined;
  requireThat(validator && validator(value), 'invalid_arguments', 'Value does not match its native projection/AgentManager contract.');
}
`);
  return { entry, validators };
}

export function browserAgentValidation(project, distribution) {
  let compiled;
  const sources = ['agent-validation.js', 'agent-validation.ts'].map(file => resolve(project, 'packages/contracts/src', file));
  return {
    name: 'ivy-browser-agent-validation',
    enforce: 'pre',
    resolveId(source, importer) {
      if (!importer || !sources.includes(resolve(dirname(importer.split('?')[0]), source))) return;
      compiled ??= writeBrowserAgentValidation(project, resolve(distribution, '.build-cache/browser-agent-validation'));
      return compiled.entry;
    },
  };
}
