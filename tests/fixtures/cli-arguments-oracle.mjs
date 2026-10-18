import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
import {parseArgs} from 'node:util';
import {IvyError, requireThat} from '../../packages/contracts/src/errors.ts';
const source=readFileSync(new URL('../../packages/cli/src/main.ts',import.meta.url),'utf8');
const start=source.indexOf('    let parsed:');
const end=source.indexOf('    // Both source and compiled');
if(start<0||end<start)throw Error('Original CLI admission boundary changed');
const admit=new Function('args','environment','defaultHostConfigPath','parseArgs','IvyError','requireThat',stripTypeScriptTypes(source.slice(start,end),{mode:'transform'})+'\nreturn {command,values,waitMs,configPath};');
const catchStart=source.indexOf('    const failure = error instanceof SyntaxError');
const catchEnd=source.indexOf('  } finally',catchStart);
if(catchStart<0||catchEnd<catchStart)throw Error('Original CLI error boundary changed');
const failure=new Function(stripTypeScriptTypes('function originalFailure(error,accepted,IvyError){'+source.slice(catchStart,catchEnd)+'}',{mode:'transform'})+'\nreturn originalFailure;')();
const rows=[];
function add(args,env,defaultPath='default.json'){
 let calls=0,result;
 try{result={ok:true,value:admit(args,env===undefined?{}:{IVY_HOST_CONFIG:env},()=>{calls++;return defaultPath;},parseArgs,IvyError,requireThat)};}
 catch(e){result={ok:false,error:IvyError.from(e).toWire(),result:failure(e,null,IvyError)};}
 rows.push({args:JSON.stringify(args),env:JSON.stringify(env??null),defaultPath:JSON.stringify(defaultPath),calls,result:JSON.stringify(result)});
}
const commands=['prepare','deploy','rollback','status','restart','enable','disable','preparations','compact-preparations','collect-storage','backup','restore','runtime-reset','runtime-reset-host','inspect-bootstrap','update-bootstrap','bootstrap-status','install-bootstrap','unknown','toString','constructor','__proto__','\ud800',''];
const options=['config','json','component','source','instance','candidate','operation-id','deployment','to','wait-ms','destination','backup','backup-hash','request','apply','confirm','phase','reset-id','runtime-epoch','scope-roots','publisher-digest','unknown','__proto__'];
for(const command of commands){
 add([command]);
 for(const option of options){for(const style of [0,1,2])add(style===0?[command,'--'+option]:style===1?[command,'--'+option,'0']:[command,'--'+option+'=0']);}
}
for(const value of ['', ' ', '\ufeff16\u2029','0x10','0o10','0b10','01','1e2','1e-9999','-1e-9999','-0','+0','600000','600001','599999.99999999999','1.1','NaN','Infinity','inf','-1','+0x1','\ud800','1_000','1n','0x','0b2']) {
 add(['deploy','--wait-ms='+value]);add(['deploy','--wait-ms',value]);
}
for(const args of [[],['status','status'],['--'],['--','status'],['status','--'],['--','--json'],['status','-'],['status','-j'],['status','---'],['status','--no-json'],['status','--json=true'],['status','--json','--json'],['status','--config=a','--config=b'],['status','--config','-'],['status','--config=\ud800'],['--config=a','status'],['status','--config='],['status','--config','--'],['status','--json','false'],['status','--config=a=b'],['status','--config','-x']])add(args);
for(const env of [undefined,'','env.json','\ud800'])for(const path of ['','default.json','\udfff'])for(const args of [['status'],['status','--config='],['status','--config=x']])add(args,env,path);
const failures=[];
for(const code of ['invalid_arguments','needs_attention','internal_error','other','\ud800'])for(const outcome of ['not_executed','unknown','completed'])for(const details of [undefined,null,{x:'\udfff'},[1,false]])for(const id of [null,'d','\ud800']){
 const message='message\ud800';const error=new IvyError(code,message,outcome,details);
 failures.push({code:JSON.stringify(code),message:JSON.stringify(message),outcome,details:details===undefined?null:JSON.stringify(details),id:JSON.stringify(id),expected:JSON.stringify(failure(error,id===null?null:{record:{deploymentId:id}},IvyError))});
}
const malformed=[null,'d'].map(id=>({id,expected:JSON.stringify(failure(new SyntaxError(),id===null?null:{record:{deploymentId:id}},IvyError))}));
process.stdout.write(JSON.stringify({rows,failures,malformed}));
