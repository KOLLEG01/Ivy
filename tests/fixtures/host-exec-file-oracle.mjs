import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {readFileSync} from 'node:fs';
const input=JSON.parse(readFileSync(0,'utf8'));
try {
  const result=await promisify(execFile)(input.file,input.args,{cwd:input.cwd,env:input.env??process.env,encoding:'utf8',maxBuffer:input.maximum,timeout:input.timeout,windowsHide:true});
  process.stdout.write(JSON.stringify({stdout:result.stdout,stderr:result.stderr,failure:null}));
} catch(error) {
  process.stdout.write(JSON.stringify({stdout:error.stdout??'',stderr:error.stderr??'',failure:error.code==='ERR_CHILD_PROCESS_STDIO_MAXBUFFER'?'maxBuffer':error.killed?'timeout':typeof error.code==='number'?'exit':'spawn'}));
}
