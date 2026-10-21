// Explicit unbound candidate seams. No socket constructor or business policy.
import {AsyncLocalStorage} from 'node:async_hooks';
import {createHash} from 'node:crypto';
import {BufferJSON,initAuthCreds,proto} from 'baileys';
import {makeLibSignalRepository} from 'baileys/lib/Signal/libsignal.js';
export const encode=value=>JSON.stringify(value,BufferJSON.replacer)??'null';
export const decode=value=>JSON.parse(value,BufferJSON.reviver);
export const digest=value=>'sha256:'+createHash('sha256').update(value).digest('hex');
const sqlText=value=>Buffer.from(value,'utf8').toString('utf8');
const names=new Set(['TypeError','SessionError','MessageCounterError','PreKeyError']);
const safeName=error=>names.has(error?.name)?error.name:'Error';
const refused=()=>Object.assign(Error('Candidate work was not admitted.'),{code:'auth_retiring'});

export function candidateAuthority(request,fence){
  const context=new AsyncLocalStorage(),active=new Set(),reservations=new Set();
  let closing=false,runtime=null,cleanup=null,cleanupUsed=false,refusedFresh=0;
  const valid=ctx=>!!ctx&&ctx.active&&active.has(ctx);
  function current(){const ctx=context.getStore();if(!valid(ctx)){fence();throw Error('Missing finite candidate authority.');}return ctx;}
  function fresh(){refusedFresh++;throw refused();}
  async function rpc(method,args){const result=await request(method,args);if(result?.refused)throw refused();return result;}
  async function run(grant,work,extra={}){
    const ctx={...grant,...extra,active:true,children:new Set(),ordinal:0};active.add(ctx);
    try{return await context.run(ctx,async()=>{const value=await work(ctx);while(ctx.children.size)await Promise.all([...ctx.children]);return value;});}
    finally{ctx.active=false;active.delete(ctx);}
  }
  async function terminal(grant,work,extra={}){
    let result,error;
    await run(grant,async()=>{try{result=await work();}catch(e){error=e;}},extra);
    await rpc('finish',{...grant,outcome:error?'library_error':'value',digest:digest(encode(error?{name:safeName(error)}:result)),name:error?safeName(error):null});
    if(error)throw error;return result;
  }
  async function spanImpl(kind,method,work){
    const ctx=context.getStore();
    if(closing){
      if(valid(ctx)&&ctx.reservedSpan&&!ctx.claimed&&ctx.expectedKind===kind&&ctx.expectedMethod===method){ctx.claimed=true;return work();}
      return fresh();
    }
    if(ctx&&!valid(ctx)){fence();throw Error('Expired candidate context.');}
    const parent=ctx??runtime;if(!parent)throw Error('No admitted runtime root.');
    const grant=await rpc('begin',{kind,method,parent:{id:parent.id,token:parent.token}});
    return terminal(grant,work);
  }
  function span(kind,method,work){
    const parent=context.getStore(),result=spanImpl(kind,method,work);
    if(valid(parent)){parent.children.add(result);result.then(()=>parent.children.delete(result),()=>parent.children.delete(result));}
    return result;
  }
  async function reserve(kind,method){
    if(closing)return fresh();const parent=current();
    const grant=await rpc('reserve',{kind,method,parent:{id:parent.id,token:parent.token}});
    const ticket={grant,kind,method,parent,used:false,ready:false};reservations.add(ticket);return ticket;
  }
  async function continuation(ticket,work){
    if(!reservations.has(ticket)||ticket.used||!valid(ticket.parent))return fresh();
    ticket.used=true;reservations.delete(ticket);
    await rpc('activate',{...ticket.grant,method:ticket.method});
    return terminal(ticket.grant,work,{reservedSpan:true,expectedKind:ticket.kind,expectedMethod:ticket.method,claimed:false});
  }
  async function cancel(ticket){
    if(!ticket.used&&reservations.delete(ticket)){ticket.used=true;await rpc('cancel',ticket.grant);}
  }
  async function rawDirect(kind,entries){
    const ctx=current(),encoded=encode(entries);
    return rpc('raw',{id:ctx.id,token:ctx.token,ordinal:String(++ctx.ordinal),kind,encoded,digest:digest(encoded)});
  }
  async function raw(kind,entries){
    const ctx=context.getStore();
    if(valid(ctx)&&ctx.cleanup)return rawDirect(kind,entries);
    if(kind==='set'&&valid(ctx)&&ctx.commitTicket?.ready&&!ctx.commitTicket.used){
      return continuation(ctx.commitTicket,()=>rawDirect(kind,entries));
    }
    if(closing){fence();throw Error('No registered raw continuation after close.');}
    return span('raw-store',kind,()=>rawDirect(kind,entries));
  }
  return {
    run,span,reserve,continuation,cancel,raw,safeName,
    current(){const ctx=current();return {id:ctx.id,token:ctx.token};},
    journalMessage(key){return span('inbound-crypto','candidate.event',()=>rpc('event',{...this.current(),value:{type:'journal.getMessage',key}}));},
    setRuntime(grant){if(runtime||closing)throw Error('Repeated candidate runtime root.');runtime=Object.freeze({...grant});},
    async transaction(original,work,key){
      return span('repository','keys.transaction',async()=>{
        const ctx=current(),ticket=await reserve('raw-store','transaction.commit');ctx.commitTicket=ticket;
        try{return await original(async()=>{const result=await work();ticket.ready=true;return result;},key);}
        finally{await cancel(ticket);}
      });
    },
    closeAdmission(closeGrant,cleanupGrant){
      if(closing||!runtime||!closeGrant?.id||!cleanupGrant?.id)throw Error('Invalid candidate close owner.');
      closing=true;cleanup={close:Object.freeze({...closeGrant}),grant:Object.freeze({...cleanupGrant})};
    },
    async tokenIndexCleanup(work){
      if(!closing||!cleanup||cleanupUsed)return fresh();cleanupUsed=true;
      await rpc('activate',{...cleanup.grant,method:'token-index.flush'});
      return terminal(cleanup.grant,work,{cleanup:true});
    },
    tracking(){return {active:active.size,reservations:reservations.size,closing,cleanupUsed,refusedFresh};},
    async finishRoot(grant,value,error){await rpc('finish',{...grant,outcome:error?'library_error':'value',digest:digest(encode(error?{name:safeName(error)}:value)),name:error?safeName(error):null});},
  };
}

// Pure options seam: no constructor/version/network action is selected here.
export function candidateSocketOptions(prepared,version,logger,authority){
  return {auth:prepared.state,version,logger,browser:['Ivy ChatBridge','Desktop','0.1.0'],
    markOnlineOnConnect:true,syncFullHistory:false,shouldSyncHistoryMessage:()=>false,
    connectTimeoutMs:30000,defaultQueryTimeoutMs:30000,keepAliveIntervalMs:20000,
    makeSignalRepository:prepared.makeSignalRepository,
    getMessage:async key=>(await authority.journalMessage(key))??undefined};
}

export async function prepareRawAuth(saved,authority){
  const creds=saved?decode(saved):initAuthCreds();let repositoryCreated=false;
  const wrapped=new WeakSet();
  const raw={
    async get(type,ids){
      const rows=await authority.raw('get',ids.map(id=>[sqlText(type),sqlText(id)]));
      if(!Array.isArray(rows)||rows.length!==ids.length)throw Error('Invalid raw candidate read.');
      const result={};for(let n=0;n<ids.length;n++)if(rows[n]){let value=decode(rows[n]);if(type==='app-state-sync-key')value=proto.Message.AppStateSyncKeyData.fromObject(value);result[ids[n]]=value;}return result;
    },
    async set(data){const entries=[];for(const[category,values]of Object.entries(data))for(const[id,value]of Object.entries(values??{}))entries.push([sqlText(category),sqlText(id),value==null?null:encode(value)]);await authority.raw('set',entries);},
  };
  function makeSignalRepository(auth,logger,pn){
    if(repositoryCreated||auth.creds!==creds||auth.keys===raw||typeof auth.keys?.transaction!=='function'||wrapped.has(auth.keys))throw Error('Candidate requires the one original supplied wrapper/graph.');
    repositoryCreated=true;wrapped.add(auth.keys);
    const transaction=auth.keys.transaction.bind(auth.keys);
    auth.keys.transaction=(work,key)=>authority.transaction(transaction,work,key);
    const original=makeLibSignalRepository(auth,logger,pn);
    const result=Object.create(Object.getPrototypeOf(original),Object.getOwnPropertyDescriptors(original));
    for(const[key,value]of Object.entries(original))if(typeof value==='function')result[key]=['close','jidToSignalProtocolAddress'].includes(key)?(...args)=>value.apply(original,args):(...args)=>authority.span('repository',key,()=>value.apply(original,args));
    const lid=original.lidMapping;
    result.lidMapping=new Proxy(lid,{get(target,key){const value=Reflect.get(target,key,target);return typeof value!=='function'?value:['storeLIDPNMappings','getLIDForPN','getLIDsForPNs','getPNForLID','getPNsForLIDs'].includes(key)?(...args)=>authority.span('repository','lidMapping.'+key,()=>value.apply(target,args)):value.bind(target);}});
    return result;
  }
  function saveCredentials(){const encoded=encode(creds);return authority.span('credential-save','creds.update',()=>authority.raw('set',[['creds','default',encoded]]));}
  await saveCredentials();
  return {state:{creds,keys:raw},raw,saveCredentials,makeSignalRepository};
}
