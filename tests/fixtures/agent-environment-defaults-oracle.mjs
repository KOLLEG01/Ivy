import {readFileSync} from 'node:fs';
import {inputs} from './whatwg-url-oracle.mjs';
import {loadEnvironmentDefaults} from '../../dist/services/agent-manager/src/environment-defaults.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
const values=new Set(inputs);
for(const scheme of ['http','https','ftp','ws','wss','custom'])for(const prefix of ['', '/', '//', '///', '/prefix', '/prefix/', '/prefix//', '//other.invalid', '/a%2Fb/', '/é/😀/'])values.add(`${scheme}://u:p@example.invalid:81${prefix}?q#h`);
for(const input of ['blob:https://example.invalid/a/','blob:http://example.invalid/a//','blob:https://example.invalid/a/\u2028','blob:custom://example.invalid/a','custom:opaque/\u2028','https://example.invalid/\ud800/\udfff','https://example.invalid/ prefix/'])values.add(input);
function projection(value){return{documentHash:hashJson(value),urls:value.mcp.servers.map(({name,url})=>({name,url}))};}
const rows=[];
for(const input of values){let result;try{result={ok:true,value:projection(await loadEnvironmentDefaults('ignored fixture artifact',input))};}catch(error){result={ok:false,error:IvyError.from(error).toWire()};}rows.push({input:JSON.stringify(input),result});}
const first=await loadEnvironmentDefaults('', 'https://example.invalid/prefix/');const original=projection(first);
first.instructions.text='external mutation';first.skills.files.length=0;first.mcp.servers[0].url='https://mutation.invalid/';
const isolated=hashJson(await loadEnvironmentDefaults('different ignored artifact', 'https://example.invalid/prefix/'))===original.documentHash;
const source=readFileSync('services/agent-manager/src/environment-defaults.json');
process.stdout.write(JSON.stringify({rows,isolated,sourceHash:hashJson(JSON.parse(source)),sourceBytes:source.length}));
