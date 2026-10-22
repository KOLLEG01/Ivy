import {existsSync,readdirSync} from 'node:fs';
import {basename} from 'node:path';
import {load} from './inbox-fixtures.mjs';
const original=await load('services/secretary/src/media-video.js');
const {digest,IvyError}=await load('packages/sdk/src/node.js');
export const instances=[];
export const loadVideoConfiguration=original.loadVideoConfiguration;

// Observe the unchanged original implementation; all codec processes still run.
export class VideoPreparer extends original.VideoPreparer {
  constructor(root,configuration){
    super(root,configuration);
    this.record={directory:basename(root),configuration:structuredClone(configuration),actions:[],trace:[]};
    this.recordRoot=root;instances.push(this.record);
  }
  async command(executable,args,cwd,maximumBytes,timeoutMs){
    const entry={executable,args:args.map(arg=>arg.replaceAll(cwd,'<scratch>')),cwd:'<scratch>',maximumBytes,timeoutMs};
    this.record.trace.push(entry);
    try{const bytes=await super.command(executable,args,cwd,maximumBytes,timeoutMs);entry.result={bytes:bytes.length,hash:digest(bytes)};return bytes;}
    catch(cause){const error=IvyError.from(cause);entry.result={code:error.code,outcome:error.outcome};throw cause;}
  }
  async prepare(source,bytes,budget){
    const action={source:structuredClone(source),base64:bytes.toString('base64'),budget};
    this.record.actions.push(action);const start=this.record.trace.length;
    try{const value=await super.prepare(source,bytes,budget);action.expected={value:structuredClone(value)};return value;}
    catch(cause){const error=IvyError.from(cause);action.expected={code:error.code,outcome:error.outcome};throw cause;}
    finally{action.trace=structuredClone(this.record.trace.slice(start));action.scratch=existsSync(this.recordRoot)?readdirSync(this.recordRoot).sort():null;}
  }
}
