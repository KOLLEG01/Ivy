import {readFileSync} from 'node:fs';
import {open} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {stripTypeScriptTypes} from 'node:module';
import {IvyError} from '../../packages/contracts/src/errors.ts';
const source=readFileSync(new URL('../../packages/host-runtime/src/artifact.ts',import.meta.url),'utf8');
const start=source.indexOf('async function hashFile('),end=source.indexOf('\n\n/** The only generic archive checksum',start);
if(start<0||end<start)throw Error('Original streaming hash boundary changed');
const fileHash=new Function('open','createHash',stripTypeScriptTypes(source.slice(start,end).replace('export const fileHash','const fileHash'),{mode:'transform'})+'\nreturn fileHash;')(open,createHash);
const input=JSON.parse(readFileSync(0,'utf8'));let result;
try{result={ok:true,value:await fileHash(input.path)};}catch(error){result={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify(result));
