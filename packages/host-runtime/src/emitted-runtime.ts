import {readFile, readdir} from 'node:fs/promises';
import {join} from 'node:path';

/** Inventory literal runtime dependencies with the JavaScript build's real compiler. */
export async function emittedRuntimePackages(root: string, typescript: typeof import('typescript')): Promise<Set<string>> {
  const packages = new Set<string>(), directories = [root];
  const add = (specifier: string): void => {
    if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('node:')) return;
    const parts = specifier.split('/'), name = specifier.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0]!;
    if (/^(?:@[a-z0-9_~-][a-z0-9._~-]*\/)?[a-z0-9_~-][a-z0-9._~-]*$/i.test(name)) packages.add(name);
  };
  for (let index = 0; index < directories.length;) {
    const directory = directories[index++]!;
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) { directories.push(path); continue; }
      if (!entry.isFile() || !/\.(?:c|m)?js$/.test(entry.name)) continue;
      const source = await readFile(path, 'utf8');
      const parsed = typescript.createSourceFile(path, source, typescript.ScriptTarget.Latest, false, typescript.ScriptKind.JS);
      const visit = (node: import('typescript').Node): void => {
        if ((typescript.isImportDeclaration(node) || typescript.isExportDeclaration(node)) && node.moduleSpecifier && typescript.isStringLiteralLike(node.moduleSpecifier)) add(node.moduleSpecifier.text);
        if (typescript.isCallExpression(node) && node.arguments.length && typescript.isStringLiteralLike(node.arguments[0]!) &&
          (node.expression.kind === typescript.SyntaxKind.ImportKeyword || typescript.isIdentifier(node.expression) && node.expression.text === 'require')) add(node.arguments[0]!.text);
        typescript.forEachChild(node, visit);
      };
      visit(parsed);
    }
  }
  return packages;
}
