// Approved current context-link representation around the original worker.
// Native calls, authorization, turns and per-item sharing use its actual code.
import {load} from './inbox-fixtures.mjs';
import {appendTechnicalImport} from './assignment-import-reference.mjs';
const {IvyError}=await load('packages/sdk/src/node.js');
const key='secretary/follow-up',contexts='secretary/follow-up-context';
export function importContextLink(store,name){
 const before=store.technicalNamed(key,name);if(!before)return null;
 if(Object.hasOwn(before.value,'contextObjectId'))return{before,after:before};
 const context=store.technicalNamed(contexts,name)??store.technicalNamed(contexts,before.value.request.itemId);
 if(context&&context.value.itemId!==before.value.request.itemId)throw new IvyError('secretary_follow_up_context_missing','The selected follow-up context belongs to another item.');
 return{before,after:appendTechnicalImport(store,key,before,{...before.value,contextObjectId:context?.pin.objectId??null})};
}
export function useCurrentFollowUpContexts(worker){
 const store=worker.engine.store,named=store.technicalNamed.bind(store),amend=store.technicalAmend.bind(store),advance=worker.advance.bind(worker);
 let question;
 const read=id=>{try{return store.technicalRead(contexts,id);}catch(error){if(IvyError.from(error).code==='not_found')return null;throw error;}};
 store.technicalNamed=(k,name)=>{
  if(k===contexts&&question){
   const saved=named(key,question),id=saved?.value.contextObjectId;
   if(id!=null&&(name===question||name===saved.value.request.itemId))return read(id);
   if(name===question)return null;
  }
  return named(k,name);
 };
 store.technicalAmend=(k,current,value)=>{
  if(k===key&&Object.hasOwn(value,'contextObjectId')&&value.threadId&&!current.value.threadId){
   const retained=value.contextObjectId==null?null:read(value.contextObjectId),shared=named(contexts,value.request.itemId);
   const selected=retained?.value.threadId===value.threadId?retained:shared;
   if(selected?.value.threadId===value.threadId)value={...value,contextObjectId:selected.pin.objectId};
  }
  return amend(k,current,value);
 };
 worker.advance=async name=>{question=name;try{return await advance(name);}finally{question=undefined;}};
}
