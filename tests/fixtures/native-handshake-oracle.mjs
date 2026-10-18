import { initializedServerVersion } from '../../dist/packages/host-runtime/src/codex-daemon.js';
let input = '';
for await (const chunk of process.stdin) input += chunk;
const values = JSON.parse(input);
console.log(JSON.stringify(values.map(value => {
  try { return { version: initializedServerVersion(value) }; }
  catch (error) { return { code: error.code, message: error.message }; }
})));
