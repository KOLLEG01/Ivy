// Test-only measurement parent for the unchanged original disposable worker.
import {Worker} from 'node:worker_threads';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';

const type={html:'text/html',docx:'application/vnd.openxmlformats-officedocument.wordprocessingml.document'};
const [format,budget]=process.argv.slice(2);
const chunks=[];let size=0,worker,timer,finished=false;
async function finish(value){
  if(finished)return;finished=true;clearTimeout(timer);
  process.stdin.destroy();await worker?.terminate();
  process.stdout.end(JSON.stringify(value)+'\n');
}
timer=setTimeout(()=>void finish({code:'media_document_timeout'}),30000);
try{
  for await(const chunk of process.stdin){
    size+=chunk.length;
    if(size>32*1024*1024){await finish({code:'media_document_too_large'});break;}
    chunks.push(chunk);
  }
  if(!finished){
    worker=new Worker(pathToFileURL(join(process.env.IVY_SECRETARY_ORACLE,'services/secretary/src/document-worker.js')),{workerData:{mediaType:type[format],bytes:Buffer.concat(chunks,size),maximumInputBytes:Number(budget)},
      resourceLimits:{maxOldGenerationSizeMb:128,maxYoungGenerationSizeMb:16,stackSizeMb:4},stdout:true,stderr:true});
    worker.stdout.resume();worker.stderr.resume();
    worker.once('message',value=>void finish(value));
    worker.once('error',()=>void finish({code:'media_document_parser_failed'}));
    worker.once('exit',()=>void finish({code:'media_document_parser_failed'}));
  }
}catch{await finish({code:'media_document_invalid'});}
