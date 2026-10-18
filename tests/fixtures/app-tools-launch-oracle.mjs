import {resolveAppToolsLaunch} from '../../dist/packages/sdk/src/codex-app-tools.js';
let text='';for await(const bytes of process.stdin)text+=bytes;
const {root,cases}=JSON.parse(text);
console.log(JSON.stringify({runtimeNode:process.execPath,results:cases.map(settings=>resolveAppToolsLaunch(settings,root))}));
