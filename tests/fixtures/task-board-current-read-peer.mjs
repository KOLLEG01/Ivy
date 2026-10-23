import assert from 'node:assert/strict';

// Finite transport controls for the actual production factory. They supply
// snapshots and suspended RPC replies, not a replacement reader or engine.
export function currentReadBoundary() {
  let snapshots={},holdQuery=false;
  const waiting=new Map();
  return ({method,params,id,reply,send,emit})=>{
    if(method==='test.currentReadConfigure'){
      if(params.snapshots)snapshots=structuredClone(params.snapshots);
      if(params.holdQuery!==undefined)holdQuery=params.holdQuery===true;
      reply(id,{});return true;
    }
    if(method==='test.currentReadRelease'){
      const work=waiting.get(params.key)??[];waiting.delete(params.key);
      for(const finish of work)finish();reply(id,{released:work.length});return true;
    }
    if(method==='objects.query'){
      emit({kind:'current-query',params});
      const finish=()=>{emit({kind:'current-query-done'});reply(id,{items:[],nextCursor:null});};
      if(holdQuery){const queue=waiting.get('query')??[];queue.push(finish);waiting.set('query',queue);}
      else finish();return true;
    }
    if(method!=='objects.read')return false;
    const row=snapshots[params.objectId];assert.ok(row,'Unbound current read '+JSON.stringify(params));
    assert.deepEqual(params,{objectId:params.objectId,revision:1});
    emit({kind:'current-node',key:params.objectId,params});
    const finish=()=>{
      emit({kind:'current-node-done',key:params.objectId});
      if(row.error){const {message,...data}=row.error;send({jsonrpc:'2.0',id,error:{code:-32000,message,data}});}
      else reply(id,row.saved);
    };
    if(row.hold){const queue=waiting.get(params.objectId)??[];queue.push(finish);waiting.set(params.objectId,queue);}
    else finish();return true;
  };
}
