import {readFileSync,readdirSync,existsSync} from 'node:fs';
import {registerHooks} from 'node:module';import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {NativeContract}=await import('../../packages/contracts/src/native-contract.ts');
const {nativeThreadState,nativeTurnItems}=await import('../../packages/sdk/src/native-observations.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const capture=fn=>{try{return {ok:true,value:fn()};}catch(e){return {ok:false,error:IvyError.from(e).toWire()};}};
const rows=[];
const states=[null,{},[],{thread:null},{thread:[]},{thread:{}},{thread:{status:null}},{thread:{status:{}}},{thread:{status:{type:42}}}];
for(const state of ['idle','systemError','active','notLoaded','', '\ud800'])for(const flag of [undefined,true,false,null,0,1,'true'])states.push({thread:{id:'thread',status:{type:state},...(flag===undefined?{}:{canAcceptDirectInput:flag}),extra:{nested:'value'}}});
const pages=[null,[],{}, {data:null},{data:{}},{data:[]},{data:[null]},{data:[[]]}, {data:[{id:'i',type:'message',text:'\ud800'}]}, {data:[{turnId:'t',item:{id:'i',type:'message'}}]}, {data:[{turnId:'other',item:{}}]}, {data:[{turnId:'t',item:null}]}, {data:[{turnId:'t',item:[]}]}, {data:[{turnId:'t',item:{}},{turnId:'other',item:{}}]}, {data:[{turnId:'\ud800',item:{}}]}];
function collect(catalog,source){const contract=new NativeContract(catalog),calls=[];
 for(const input of states)calls.push({kind:'thread',input:JSON.stringify(input),expected:JSON.stringify(capture(()=>nativeThreadState(contract,input)))});
 for(const input of pages)for(const turnId of ['t','','\ud800'])calls.push({kind:'items',input:JSON.stringify(input),turnId:JSON.stringify(turnId),expected:JSON.stringify(capture(()=>nativeTurnItems(contract,input,turnId)))});
 rows.push({...source,calls});
}
for(const dir of readdirSync('specs/native').filter(s=>/^codex-/.test(s)))for(const file of readdirSync('specs/native/'+dir).filter(s=>/^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(s))){const path='specs/native/'+dir+'/'+file;collect(JSON.parse(readFileSync(path,'utf8')),{path});}
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const method=catalog.clientRequests[0];
const object=properties=>({type:'object',properties});
const directItem={...object({id:{},type:{}}),required:['id','type']};
const wrapper={...object({turnId:{},item:{}}),required:['turnId','item']};
const thread=flag=>object({thread:object(flag?{canAcceptDirectInput:false}:{})});
const items=item=>object({data:{type:'array',items:item}});
function synthetic(label,read,listing){const c={...catalog,clientRequests:[...(read===undefined?[]:[{...method,method:'thread/read',inputSchema:true,outputSchema:read}]),...(listing===undefined?[]:[{...method,method:'thread/items/list',inputSchema:true,outputSchema:listing}])],serverRequests:[],serverNotifications:[],clientNotifications:[],derivedLegacyTypes:[]};collect(c,{label,source:JSON.stringify(c)});}
synthetic('missing-methods');
for(const read of [true,false,{},object({}),object({thread:true}),object({thread:{type:'array'}}),object({thread:{type:'object',properties:[]}}),thread(false),thread(true),{$ref:'#/$defs/Absent',$defs:{}},{$ref:'#/$defs/constructor',$defs:{}},{$ref:'#/$defs/A',$defs:[]},{$ref:42},{$ref:'external'},{$ref:'#/$defs/a_b',$defs:{a_b:thread(true)}},{$ref:'#/$defs/A',$defs:{A:{$ref:'#/$defs/A'}}}])synthetic('thread-schema',read,items({oneOf:[directItem]}));
for(const item of [true,false,{},wrapper,{...wrapper,required:['item']},object({item:{},turnId:{}}),{oneOf:[directItem]},{anyOf:[directItem]},{oneOf:null,anyOf:[directItem]},{oneOf:[],anyOf:[directItem]},{oneOf:false,anyOf:[directItem]},{oneOf:[null]},{oneOf:[true]},{oneOf:[object({})]},{oneOf:[{...directItem,required:['id']}]},{oneOf:[directItem,directItem]},{$ref:'#/$defs/Absent'}])synthetic('item-schema',thread(true),items(item));
for(const listing of [true,{},object({}),object({data:true}),object({data:{type:'object'}}),object({data:{type:'array'}})])synthetic('page-schema',thread(false),listing);
for(const size of [1,63,64,65]){const defs={};for(let i=0;i<size;i++)defs['N'+i]=i+1===size?thread(true):{$ref:'#/$defs/N'+(i+1)};synthetic('reference-depth-'+size,{$ref:'#/$defs/N0',$defs:defs},items(wrapper));}
process.stdout.write(JSON.stringify(rows));
