import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);

const texts = [
  { component: 'secretary', name: 'secretary-notification-policy', sourcePath: 'instructions/secretary-notification-policy.ts', exportName: 'secretaryNotificationPolicy' },
  { component: 'chat-bridge', name: 'chat-main-instructions', sourcePath: 'services/chat-bridge/src/main-context.ts', exportName: 'mainInstructions' },
  { component: 'chat-bridge', name: 'chat-notice-prompt', sourcePath: 'services/chat-bridge/src/notice-input.ts', exportName: 'noticePrompt', template: true },
];

function literalText(path, exportName, template) {
  const ts = require('typescript');
  const source = ts.createSourceFile(path, readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true);
  if (source.parseDiagnostics.length) throw new Error('Invalid instruction source: ' + path);
  const declarations = source.statements.filter(statement =>
    ts.isVariableStatement(statement) &&
    statement.modifiers?.some(modifier => modifier.kind === ts.SyntaxKind.ExportKeyword) &&
    (statement.declarationList.flags & ts.NodeFlags.Const)
  ).flatMap(statement => [...statement.declarationList.declarations])
    .filter(declaration => ts.isIdentifier(declaration.name) && declaration.name.text === exportName);
  const value = declarations.length === 1 ? declarations[0].initializer : undefined;
  if (template) {
    const body = value && ts.isArrowFunction(value) ? value.body : undefined;
    const spans = body && ts.isTemplateExpression(body) ? body.templateSpans : [];
    const text = spans[0]?.expression, prefix = spans[1]?.expression;
    const parameters = value && ts.isArrowFunction(value) ? value.parameters : [];
    if (parameters.length !== 2 || !parameters.every((parameter, index) => ts.isIdentifier(parameter.name) && parameter.name.text === ['callerPrincipalId', 'text'][index] && !parameter.initializer && !parameter.dotDotDotToken) ||
      value.modifiers?.some(modifier => modifier.kind === ts.SyntaxKind.AsyncKeyword) ||
      spans.length !== 2 || !text || !ts.isIdentifier(text) || text.text !== 'text' ||
      !prefix || !ts.isCallExpression(prefix) || prefix.questionDotToken || prefix.typeArguments?.length ||
      !ts.isIdentifier(prefix.expression) || prefix.expression.text !== 'noticePrefix' || prefix.arguments.length !== 1 ||
      !ts.isIdentifier(prefix.arguments[0]) || prefix.arguments[0].text !== 'callerPrincipalId') {
      throw new Error('Instruction template must contain exactly text and noticePrefix(callerPrincipalId): ' + exportName);
    }
    return [body.head.text, ...spans.map(span => span.literal.text)];
  }
  if (!value || !(ts.isStringLiteral(value) || ts.isNoSubstitutionTemplateLiteral(value)))
    throw new Error('Instruction export must be one constant string literal: ' + exportName);
  // TypeScript's cooked literal text follows JavaScript escape and line-ending
  // rules, retaining unpaired UTF-16 units. Never execute the source module.
  return value.text;
}

/** Emit shared text as JSON strings or ordered template parts under <outputRoot>/instructions.
 * Rust consumers use ivy_contracts::json::parse and retain IvyJson::String;
 * they must not convert the value through a UTF-8-only JSON string type. */
export function buildInstructions({ root = process.cwd(), outputRoot = resolve(root, 'dist'), component = null } = {}) {
  root = realpathSync(root);
  const selected = texts.filter(text => component === null || text.component === component);
  const values = selected.map(text => {
    const source = resolve(root, text.sourcePath);
    if (realpathSync(source) !== source) throw new Error('Instruction sources must not use directory or file links.');
    return { ...text, value: literalText(source, text.exportName, text.template) };
  });
  if (!values.length) return [];
  const directory = resolve(outputRoot, 'instructions');
  mkdirSync(directory, { recursive: true });
  if (realpathSync(directory) !== directory) throw new Error('Instruction output must not use directory links.');
  const files = values.map(text => resolve(directory, text.name + '.json'));
  for (const file of files)
    if (existsSync(file) && realpathSync(file) !== file) throw new Error('Instruction output must not use file links.');
  for (let index = 0; index < values.length; index++)
    writeFileSync(files[index], JSON.stringify(values[index].value) + '\n');
  return files;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const value = name => process.argv.includes(name) ? process.argv[process.argv.indexOf(name) + 1] : undefined;
  buildInstructions({ root: value('--root'), component: value('--component'), outputRoot: value('--out') });
}
