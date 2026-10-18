// The approved one-time mapping, driven through the original Store's real
// schema/CAS/receipt boundary. Normal reference workers then use current fields.
import {load} from './inbox-fixtures.mjs';
const {IvyError,hashJson}=await load('packages/sdk/src/node.js');
export async function importExecutionDelivery(engine,execution){
 await engine.verifyOwner();
 const before=await engine.store.read('secretary/execution',execution.pin.objectId);
 if(before.value.executionId!==execution.value.executionId)throw new IvyError('secretary_assignment_identity_conflict','The retained execution must keep its original identity.');
 const next=structuredClone(before.value),mappings=[];
 for(const [sourceKey,targetField] of [['secretary/assignment-handoff','delivery'],['secretary/assignment-voice','voice']]){
  const source=engine.store.technicalNamed(sourceKey,next.executionId);if(!source)continue;
  const applied=next[targetField]==null;
  if(applied){
   const voice=targetField==='voice',fail=()=>{throw new IvyError(voice?'secretary_voice_mismatch':'secretary_assignment_result_changed',voice?'The original Voice request changed.':'The retained decision changed after delivery began.');};
   if(source.value.executionId!==next.executionId||source.value.resultHash!==hashJson(before.value.result))fail();
   if(voice&&(source.value.source?.objectId!==before.pin.objectId||!Number.isSafeInteger(source.value.source?.revision)||source.value.source.revision<1||source.value.source.revision>before.pin.revision))fail();
   next[targetField]=structuredClone(source.value);
  }
  mappings.push({sourceKey,targetField,source,applied});
 }
 // amend validates at its actual write boundary; unchanged current fields were
 // already validated by the original read. No original record is removed.
 const after=await engine.store.amend(before,'secretary/execution',next);
 return{before,after,mappings};
}
