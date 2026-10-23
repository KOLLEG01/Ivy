import {mkdirSync,readFileSync,writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {buildNativeDispatchers} from '../../tools/build/build-native-dispatchers.mjs';
const input=JSON.parse(readFileSync(0,'utf8'));
const directory=resolve(input.root,'services',input.plan.componentId);
mkdirSync(directory,{recursive:true});
writeFileSync(resolve(directory,'deploy.json'),JSON.stringify(input.plan));
process.stdout.write(JSON.stringify(buildNativeDispatchers({component:input.plan.componentId,root:input.root,outputRoot:input.outputRoot})));
