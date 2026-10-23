import { parseArgs } from 'node:util';
import path from 'node:path';

const required = ['--config', 'config.json', '--request', 'request.json', '--distribution', 'artifact'];
const cases = [
  [], required, [...required, '--'], [...required, '--config', 'later.json'],
  ['--config=café/../配置.json', '--request=request=file.json', '--distribution=artifact'],
  ['--config=-config.json', '--request=-', '--distribution=.'],
  ['--config', '-', '--request', 'request', '--distribution', '../artifact'],
  [...required, '--config='], [...required, '--request', ''], [...required, '--distribution='],
  [...required, '--config'], [...required, '--request', '--distribution'],
  [...required, '--config', '-value'], [...required, '--bad=value'], [...required, 'positional'],
  [...required, '--', '--config=later'], ['-c', 'config'], ['--no-config', ...required],
  [...required, '--request=a=b=c'], ['--config', '--', '--request', 'r', '--distribution', 'd'],
];
const rows = cases.map(args => {
  try {
    const { values } = parseArgs({ args, options: Object.fromEntries(['config', 'request', 'distribution'].map(key => [key, { type: 'string' }])) });
    if (!values.config || !values.request || !values.distribution) return { args, code: 'invalid_arguments' };
    return { args, paths: Object.fromEntries(['config', 'request', 'distribution'].map(key => [key, path.resolve(process.argv[2], values[key])])) };
  } catch {
    // The original entrypoint maps non-Ivy parseArgs errors through IvyError.from.
    return { args, code: 'internal_error' };
  }
});
process.stdout.write(JSON.stringify(rows));
