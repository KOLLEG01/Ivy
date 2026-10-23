import {readFileSync,writeFileSync,mkdirSync,existsSync} from 'node:fs';
import {join} from 'node:path';
import {startAgentManager} from '../../dist/services/agent-manager/src/main.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
const input=JSON.parse(readFileSync(0,'utf8')),result=[];
for(const row of input.cases){
 const root=join(input.root,row.name),data=join(root,'data'),artifact=join(root,'artifact');mkdirSync(join(artifact,'dist'),{recursive:true});mkdirSync(data);
 let config={...input.config,dataRoot:data,artifactRoot:artifact};
 const build={buildId:config.buildId,version:row.name==='build-mismatch'?'other':config.version};
 writeFileSync(join(artifact,'dist/build-info.json'),JSON.stringify(build));
 if(row.name==='component')config.componentId='task-board';
 if(row.name==='credential')delete config.credential;
 if(row.name==='settings')config.settings={...config.settings,nativeVersion:false};
 if(row.name==='root-overlap')config.artifactRoot=data;
 const path=join(root,'instance.json');writeFileSync(path,JSON.stringify(config));
 const marker={schemaVersion:1,resetId:'reset',runtimeEpoch:'epoch',completedAt:row.name==='reset-unsupported'?'2026-10-19T00:00:00.000Z':'invalid'};
 writeFileSync(join(data,'runtime-reset.json'),JSON.stringify(marker));
 let reply;
 try{await startAgentManager(path);throw Error('Prelaunch fixture unexpectedly started');}catch(error){reply={ok:false,error:IvyError.from(error).toWire()};}
 result.push({name:row.name,reply,journal:existsSync(join(data,'journal')),health:existsSync(join(data,'health.json')),applied:existsSync(join(data,'runtime-reset-applied.json'))});
}
console.log(JSON.stringify(result));
