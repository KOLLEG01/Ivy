import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';

export function coordination(source, { canonical, hashJson, IvyError, requireThat, mutation, Date }) {
  const captured = readFileSync(new URL('./task-board-coordination.ts.txt', import.meta.url), 'utf8');
  const split = captured.indexOf('  private async gates(');
  const start = source.indexOf('  private async gates(');
  const end = source.indexOf('  async current(', start);
  source = source.slice(source.indexOf('export class'), start) + captured.slice(split) + source.slice(end);
  return new Function('canonical', 'hashJson', 'IvyError', 'requireThat', 'mutation', 'Date',
    stripTypeScriptTypes(captured.slice(0, split) + source.replace('export class', 'class'), { mode: 'transform' }) + '\nreturn TaskBoardCoordination;'
  )(canonical, hashJson, IvyError, requireThat, mutation, Date);
}
