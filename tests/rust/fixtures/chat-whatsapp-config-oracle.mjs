import {readFile, writeFile, mkdtemp, realpath} from 'node:fs/promises';
import {readFileSync, writeFileSync} from 'node:fs';
import {isAbsolute, join, parse} from 'node:path';
import {tmpdir} from 'node:os';
import {stripTypeScriptTypes} from 'node:module';

const transform=source=>stripTypeScriptTypes(source,{mode:'transform'});
const errors=readFileSync('packages/contracts/src/errors.ts','utf8').replace(/^export /gm,'');
const {IvyError,requireThat}=new Function(transform(errors)+'\nreturn {IvyError,requireThat};')();
const source=readFileSync('services/chat-bridge/src/whatsapp/config.ts','utf8')
  .replace(/^import .*\r?\n/gm,'').replace(/^export /gm,'');
const original=new Function('readFile','isAbsolute','requireThat',
  transform(source)+'\nreturn readWhatsAppConfig;')(readFile,isAbsolute,requireThat);

const base={accountId:'account',botPhoneNumber:'+15555550200',
  senders:[{phoneNumber:'0015555550201',principalId:'principal',channelId:'channel'}],since:'2024-01-01T00:00:00Z'};
const definition={channels:[{channel:{adapter:'whatsapp',accountId:'account',channelId:'channel'}}]};
const clone=value=>JSON.parse(JSON.stringify(value));
const cases=[];
const add=(name,change=()=>{},changeDefinition=()=>{})=>{
  const value=clone(base),currentDefinition=clone(definition);
  change(value);changeDefinition(currentDefinition);
  cases.push({name,raw:JSON.stringify(value),definition:currentDefinition});
};
const field=(name,key,value)=>add(name,current=>{current[key]=value;});
const omit=(name,key)=>add(name,current=>{delete current[key];});
add('original valid configuration');
for(const [index,value] of [null,0,true,[],{},''].entries()) {
  field('accountId '+index,'accountId',value);
}
omit('missing accountId','accountId');
add('empty accountId matches exact empty account',v=>{v.accountId='';},d=>{d.channels[0].channel.accountId='';});
for(const [index,value] of [15555550200,[15555550200],[['+15555550200']],null,false,{},
  {toString:'15555550200'},{toString:null},['15555550200',null],'+15555550200\n',
  '15555550200\r','15555550200\u2028','+01234567','1234567','123456789012345',
  '1234567890123456','１２３４５６７'].entries()) field('botPhoneNumber '+index,'botPhoneNumber',value);
omit('missing botPhoneNumber','botPhoneNumber');
for(const [index,value] of [null,{},'sender',[],[base.senders[0],base.senders[0]]].entries()) {
  field('senders '+index,'senders',value);
}
omit('missing senders','senders');
for(const [index,value] of [null,10000,120000,10000.5,9999,120001,'10000',true,-0].entries()) {
  field('heartbeatMs '+index,'heartbeatMs',value);
}
for(const [index,value] of [null,false,0,1,49,50,99,100,999,[],[0],['2024-01-01'],
  {},{toString:null},'','2024-02-30','2024-13-01','2024-01-01T24:00:00Z',
  '2024-01-01T24:00:01Z','2024-10-27T02:30:00','2024-03-31T02:30:00',
  '+275760-09-13T00:00:00.000Z','+275760-09-13T00:00:00.001Z',
  '-271821-04-20T00:00:00.000Z','-271821-04-19T23:59:59.999Z',
  '01 Jan 1970 00:00:00 GMT','2024-01-01T01:00:00+01:00','2024-01-01T00:00:00+24:00',
  '\u00002024-01-01','2024-01-01\u0000ignored','\ud800'].entries()) field('since '+index,'since',value);
omit('missing since','since');
for(const [name,change] of [
  ['account short circuits phone coercion',v=>{v.accountId=0;v.botPhoneNumber={toString:null};}],
  ['bad bot short circuits since coercion',v=>{v.botPhoneNumber=false;v.since={toString:null};}],
  ['bad sender count short circuits since coercion',v=>{v.senders=[];v.since={toString:null};}],
  ['date coercion precedes invalid heartbeat',v=>{v.since={toString:null};v.heartbeatMs=0;}],
  ['heartbeat precedes null sender',v=>{v.heartbeatMs=0;v.senders=[null];}],
]) add(name,change);
for(const [index,value] of [null,0,true,'sender',[],{},
  {phoneNumber:{toString:null}},{phoneNumber:15555550201,channelId:'channel'},
  {phoneNumber:['+15555550201'],channelId:'channel'}].entries()) field('sender '+index,'senders',[value]);
for(const [index,value] of [undefined,null,0,false,{},[],['principal']].entries()) {
  add('unvalidated principalId '+index,v=>{v.senders[0].principalId=value;});
}
for(const [index,value] of [undefined,null,0,false,{},[],['channel']].entries()) {
  add('channelId strict equality '+index,v=>{v.senders[0].channelId=value;},
    d=>{d.channels[0].channel.channelId=value;});
}
for(const [name,change] of [
  ['missing configured channel',d=>{d.channels=[];}],
  ['wrong adapter',d=>{d.channels[0].channel.adapter='WhatsApp';}],
  ['wrong account',d=>{d.channels[0].channel.accountId=0;}],
  ['wrong channel',d=>{d.channels[0].channel.channelId='other';}],
  ['missing channels',d=>{delete d.channels;}],
  ['channels is null',d=>{d.channels=null;}],
  ['channels is an object',d=>{d.channels={};}],
  ['null row before match',d=>{d.channels.unshift(null);}],
  ['null row after match is not visited',d=>{d.channels.push(null);}],
  ['missing channel',d=>{d.channels.unshift({});}],
  ['null channel',d=>{d.channels.unshift({channel:null});}],
  ['primitive channel is skipped',d=>{d.channels.unshift({channel:0});}],
  ['unmatched adapter short circuits remaining fields',d=>{d.channels.unshift({channel:{adapter:'other'}});}],
]) add(name,()=>{},change);
add('sender refusal precedes malformed definition',v=>{v.senders[0].phoneNumber='bad';},d=>{d.channels=null;});
add('unknown media fields retained',v=>{
  v.transcription={apiKey:null,baseUrl:0,model:[],language:'\ud800',unknown:{nested:true}};
  v.extra={'2':'two','1':'one','01':'leading','__proto__':'plain',valueOf:false};
});
field('transcription is null and remains null','transcription',null);
cases.push({name:'negative zero and nonfinite JSON values retained',
  raw:JSON.stringify(base).slice(0,-1)+',"extra":[-0,1e400,-1e400]}',definition});
cases.push({name:'duplicate JSON keys use last value',
  raw:'{"accountId":0,'+JSON.stringify(base).slice(1),definition});
cases.push({name:'large unknown field has no added policy limit',
  raw:JSON.stringify({...base,extra:'x'.repeat(262145)}),definition});
for(const [name,raw] of [['null configuration','null'],['false configuration','false'],
  ['number configuration','1'],['string configuration','"text"'],['array configuration','[]'],
  ['empty JSON',''],['truncated JSON','{"accountId":'],['trailing data',JSON.stringify(base)+' x'],
  ['BOM is not removed','\ufeff'+JSON.stringify(base)],['invalid JSON escape','{"x":"\\uZZZZ"}']]) {
  cases.push({name,raw,definition});
}
const root=await mkdtemp(join(tmpdir(),'ivy-chat-config-'));
let index=0;
for(const row of cases) {
  row.path=join(root,String(index++)+'.json');
  const bytes=Buffer.from(row.raw,'utf8');row.rawHex=bytes.toString('hex');delete row.raw;
  await writeFile(row.path,bytes);
}
for(const [name,bytes] of [
  ['malformed UTF-8 becomes replacement characters',Buffer.concat([
    Buffer.from(JSON.stringify(base).slice(0,-1)+',"extra":"'),
    Buffer.from([0xf0,0x9f,0x80,0x20,0xed,0xa0,0x80,0xc0,0xaf,0xff]),Buffer.from('"}')])],
  ['truncated UTF-8 remains parseable inside string',Buffer.concat([
    Buffer.from(JSON.stringify(base).slice(0,-1)+',"extra":"'),Buffer.from([0xe2,0x82]),Buffer.from('"}')])],
]) {
  const path=join(root,String(index++)+'.json');await writeFile(path,bytes);
  cases.push({name,path,rawHex:bytes.toString('hex'),definition});
}
const validPath=cases[0].path;
cases.push({name:'missing file',path:join(root,'missing.json'),definition},
  {name:'directory instead of file',path:root,definition},
  {name:'NUL absolute path',path:validPath+'\0',definition});
for(const path of ['', '.', 'relative.json', 'C:relative.json', '1:/relative.json']) {
  cases.push({name:'nonabsolute protected path '+JSON.stringify(path),path,definition});
}
if(process.platform==='win32') {
  cases.push({name:'Windows forward slashes',path:validPath.replaceAll('\\','/'),definition},
    {name:'Windows rooted path without drive',path:validPath.slice(parse(validPath).root.length-1),definition});
}
const surrogatePath=join(root,'lone-\ud800.json');await writeFile(surrogatePath,JSON.stringify(base));
cases.push({name:'filesystem replaces unpaired UTF-16 in path',path:surrogatePath,
  rawHex:Buffer.from(JSON.stringify(base)).toString('hex'),definition});

// Encode JSON numbers and object ordering explicitly: JSON.stringify alone
// would erase negative zero and nonfinite values that the reader must retain.
function describe(value) {
  if(value===null)return ['null'];
  if(typeof value==='number')return ['number',Object.is(value,-0)?'-0':String(value)];
  if(Array.isArray(value))return ['array',value.map(describe)];
  if(typeof value==='object')return ['object',Object.entries(value).map(([key,v])=>[key,describe(v)])];
  return [typeof value,value];
}
for(const row of cases) {
  try { row.expected={value:describe(await original(row.path,row.definition))}; }
  catch(error) { row.expected={error:IvyError.from(error).toWire()}; }
}
const result={root,canonicalRoot:await realpath(root),platform:process.platform,node:process.version,cases};
if(process.env.IVY_CHAT_CONFIG_ORACLE_CAPTURE)writeFileSync(process.env.IVY_CHAT_CONFIG_ORACLE_CAPTURE,JSON.stringify(result));
process.stdout.write(JSON.stringify(result));
