import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {resolve,dirname,join} from 'node:path';import {pathToFileURL} from 'node:url';
import {oracleRoot,load} from './inbox-fixtures.mjs';
const {IvyError}=await load('packages/sdk/src/node.js');
const [artifact,version]=process.argv.slice(2);
const original=resolve(oracleRoot,'packages/contracts/src/checked-native-contract.js');
// Preserve the exact original loader and its dist-relative import.meta.url.
// Only its fixed dependency imports point back at the frozen oracle modules.
const source=(await readFile(original,'utf8')).replace(/from (['"])(\.[^'"]+)\1/g,(_,quote,specifier)=>'from '+JSON.stringify(pathToFileURL(resolve(dirname(original),specifier)).href));
const copy=join(artifact,'dist/packages/contracts/src/checked-native-contract.mjs');await mkdir(dirname(copy),{recursive:true});await writeFile(copy,source);
try{const {checkedNativeContract}=await import(pathToFileURL(copy));const selected=checkedNativeContract(version);
 console.log(JSON.stringify({catalogHash:selected.catalogHash,sourceHash:selected.catalog.sourceHash,nativeExecutableHash:selected.catalog.nativeExecutableHash}));
}catch(cause){const error=IvyError.from(cause);console.log(JSON.stringify({code:error.code,outcome:error.outcome}));}
