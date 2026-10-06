import type { RpcClient, Result } from './client.js';
import { ServiceConnection } from './service.js';
type Owner=Result<'serviceNodes.get'>;
const owners=new WeakMap<ServiceConnection,Promise<Owner>>();
/** Bound ownership, not live status. Authenticate the owner once per ServiceConnection. */
export async function connectionOwner(client:RpcClient,serviceNodeId:string):Promise<Owner>{
  if(!(client instanceof ServiceConnection)||client.serviceNodeId!==serviceNodeId)return client.request('serviceNodes.get',{serviceNodeId});
  client.signal.throwIfAborted();let pending=owners.get(client);
  if(!pending){
    pending=client.request('serviceNodes.get',{serviceNodeId});owners.set(client,pending);
    void pending.then(node=>{if(!node.connected||!node.synced)owners.delete(client);},()=>owners.delete(client));
  }
  const node=await pending;client.signal.throwIfAborted();return structuredClone(node);
}
