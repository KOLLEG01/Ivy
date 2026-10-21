// NAB-3P repository seam. No socket constructor, network or business backend.
import {AsyncLocalStorage} from 'node:async_hooks';
import {createHash} from 'node:crypto';
import {BufferJSON, initAuthCreds, proto, addTransactionCapability, DEFAULT_CONNECTION_CONFIG} from 'baileys';
import {makeLibSignalRepository} from 'baileys/lib/Signal/libsignal.js';

export const encode = value => JSON.stringify(value, BufferJSON.replacer) ?? 'null';
export const decode = value => JSON.parse(value, BufferJSON.reviver);
export const digest = text => 'sha256:' + createHash('sha256').update(text).digest('hex');
const sqlText = value => Buffer.from(value,'utf8').toString('utf8');
const quiet = {trace(){},debug(){},info(){},warn(){},error(){},child(){return this;}};
const safeName = error => ['TypeError','SessionError','MessageCounterError','PreKeyError'].includes(error?.name) ? error.name : 'Error';

export function protocolAuthority(request, fence) {
  const context = new AsyncLocalStorage(), active = new Set();
  let closing = false;
  const valid = ctx => !!ctx && ctx.active && active.has(ctx);
  function check(ctx) { if (!valid(ctx)) { fence(); throw Error('Unproved authentication context.'); } }
  async function run(grant, work) {
    const ctx = {...grant,active:true,ordinal:0,children:new Set()};
    active.add(ctx);
    try {
      const result = await context.run(ctx, () => work(ctx));
      while (ctx.children.size) await Promise.all([...ctx.children]);
      return result;
    } finally { ctx.active=false; active.delete(ctx); }
  }
  async function span(kind, method, work) {
    const parent = context.getStore();
    if (parent) check(parent);
    else if (closing) throw Error('Authentication root admission is closed.');
    const grant = await request('begin',{kind,method,parent:parent ? {id:parent.id,token:parent.token} : null});
    return run(grant, async ctx => {
      let result, error;
      try { result=await work(); }
      catch(e) { error=e; }
      while (ctx.children.size) await Promise.all([...ctx.children]);
      // This records this method's return, not its enclosing transaction commit.
      const encoded=encode(error ? {name:safeName(error)} : result);
      await request('finish',{id:grant.id,token:grant.token,outcome:error?'library_error':'value',digest:digest(encoded),name:error?safeName(error):null});
      if(error) throw error;
      return result;
    });
  }
  function owned(kind,method,work) {
    const parent=context.getStore();
    if(parent)check(parent);
    const result=span(kind,method,work);
    if(parent){parent.children.add(result);result.then(()=>parent.children.delete(result),()=>parent.children.delete(result));}
    return result;
  }
  return {
    run,span:owned,
    current(){const ctx=context.getStore();check(ctx);return {id:ctx.id,token:ctx.token};},
    closeAdmission(){closing=true;},
    active(){return active.size;},
    async raw(kind, entries) {
      const ctx=context.getStore();
      if(!ctx) return owned('raw-store',kind,()=>this.raw(kind,entries));
      check(ctx);
      const encoded=encode(entries),ordinal=String(++ctx.ordinal);
      return request('raw',{id:ctx.id,token:ctx.token,ordinal,kind,encoded,digest:digest(encoded)});
    },
    async finishRoot(grant,result,error) {
      await request('finish',{id:grant.id,token:grant.token,outcome:error?'library_error':'value',digest:digest(encode(error?{name:safeName(error)}:result)),name:error?safeName(error):null});
    },
    safeName,
  };
}

export async function createProtocolAuth(saved, authority, pnFromLIDUSync) {
  const creds=saved ? decode(saved) : initAuthCreds();
  const raw={
    async get(type,ids) {
      const rows=await authority.raw('get',ids.map(id=>[sqlText(type),sqlText(id)]));
      if(!Array.isArray(rows)||rows.length!==ids.length)throw Error('Invalid raw authentication response.');
      const result={};
      for(let i=0;i<ids.length;i++)if(rows[i]){
        let value=decode(rows[i]);
        if(type==='app-state-sync-key')value=proto.Message.AppStateSyncKeyData.fromObject(value);
        result[ids[i]]=value;
      }
      return result;
    },
    async set(data) {
      const entries=[];
      for(const[category,values]of Object.entries(data))for(const[id,value]of Object.entries(values??{}))entries.push([sqlText(category),sqlText(id),value==null?null:encode(value)]);
      await authority.raw('set',entries);
    },
  };
  // Source-mapped socket.js:267-268: raw keys, exactly one original wrapper.
  const keys=addTransactionCapability(raw,quiet,DEFAULT_CONNECTION_CONFIG.transactionOpts);
  const transaction=keys.transaction.bind(keys);
  keys.transaction=(work,key)=>authority.span('repository','keys.transaction',()=>transaction(work,key));
  function makeSignalRepository(auth,logger,pn) {
    const original=makeLibSignalRepository(auth,logger,pn);
    const result=Object.create(Object.getPrototypeOf(original),Object.getOwnPropertyDescriptors(original));
    for(const[key,value]of Object.entries(original))if(typeof value==='function'){
      result[key]=['close','jidToSignalProtocolAddress'].includes(key)
        ? (...args)=>value.apply(original,args)
        : (...args)=>authority.span('repository',key,()=>value.apply(original,args));
    }
    const lid=original.lidMapping;
    result.lidMapping=new Proxy(lid,{get(target,key){const value=Reflect.get(target,key,target);return typeof value!=='function'?value:['storeLIDPNMappings','getLIDForPN','getLIDsForPNs','getPNForLID','getPNsForLIDs'].includes(key)
      ? (...args)=>authority.span('repository','lidMapping.'+key,()=>value.apply(target,args)) : value.bind(target);}});
    return result;
  }
  const state={creds,keys},repository=makeSignalRepository(state,quiet,pnFromLIDUSync);
  async function saveCredentials(){
    // Freeze the complete object now, not a later partial-event reconstruction.
    const encoded=encode(creds);
    await authority.span('credential-save','creds.update',()=>authority.raw('set',[['creds','default',encoded]]));
  }
  await saveCredentials();
  return {state,repository,raw,saveCredentials,makeSignalRepository};
}
