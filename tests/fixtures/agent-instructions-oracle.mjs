import { composeInstructions } from '../../dist/services/agent-manager/src/instructions.js';
let text = '';
for await (const bytes of process.stdin) text += bytes;
const result = JSON.parse(text).map(({global, host, local, defaults, discovered}) => {
  try {
    const value = composeInstructions(global, host, local === null ? null : Buffer.from(local), defaults, discovered);
    return { body: value.body === null ? null : value.body.toString('utf8'), version: value.version };
  } catch (error) { return { code: error.code, outcome: error.outcome }; }
});
console.log(JSON.stringify(result));
