import {existsSync,readFileSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {HostJournal}=await import('../../packages/host-runtime/src/journal.ts');
const {saveAcceptedConfiguration}=await import('../../packages/host-runtime/src/accepted-configuration.ts');
const {atomicJson,instanceConfig}=await import('../../packages/host-runtime/src/config.ts');
const {canonical}=await import('../../packages/contracts/src/canonical.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
const journal=new HostJournal(input.host);
try{
  let reference=null;
  if(input.mode==='seed'){
    for(const [candidate,manifest]of input.candidates){
      journal.saveCandidate(candidate,manifest);
      await atomicJson(candidate.manifestPath,manifest);
      await atomicJson(candidate.artifactRoot+'/dist/build-info.json',{schemaVersion:1,buildId:candidate.buildId,version:manifest.version});
    }
    for(const [path,value]of input.configurations){validateHost('InstanceConfig',value);await atomicJson(path,value);}
    for(const target of input.targets.slice(0,3))journal.restoreTarget(target);
    for(const [index,id]of ['fixture','bootstrap-fixture'].entries()){
      const entry=journal.accept({action:'deploy',operationId:'retained-'+index,instanceId:id,candidateId:input.targets[0].candidateId});
      entry.previousCandidateId=input.targets[3+index].candidateId;
      entry.activation.previousTarget=input.targets[3+index];
      entry.record.phase='succeeded';validateHost('JournalEntry',entry);
      journal.db.prepare("UPDATE operations SET phase='succeeded',entry_json=? WHERE operation_id=?").run(canonical(entry),'retained-'+index);
    }
    journal.setInstalled({instanceId:'fixture',candidateId:input.targets[0].candidateId,buildId:input.targets[0].candidateId,enabled:true,installedAt:'2026-01-01T00:00:00.000Z'});
    journal.db.prepare('INSERT INTO meta VALUES (?,?)').run('bootstrap-rollback:bootstrap-fixture',canonical({candidateId:input.targets[4].candidateId,selectedCandidateId:input.targets[2].candidateId}));
    journal.db.prepare('INSERT INTO meta VALUES (?,?)').run('recovery-original','preserve recovery evidence');
    reference=saveAcceptedConfiguration(input.host);
  }
  const targets=['fixture','stopped-fixture','bootstrap-fixture'].map(id=>journal.target(id));
  const rollback=[0,1].map(index=>journal.operation('retained-'+index).activation.previousTarget);
  const configurations=[];for(const target of [...targets,...rollback])configurations.push(await instanceConfig(target.configPath));
  process.stdout.write(JSON.stringify({reference,targets,rollback,configurations,protectedCandidates:[...journal.retentionCandidateIds()].sort()}));
}finally{journal.close();}
