// Shared test boundaries; the application and SDK sources execute unchanged.
import {starting,now,instructions,noticeInstructions} from './chat-main-oracle.mjs';
import {registerHooks} from 'node:module';
const url=p=>new URL(p,import.meta.url).href;
const data=s=>'data:text/javascript;base64,'+Buffer.from(s).toString('base64');
const key=Symbol.for('ivy.test.chat-service-runtime');
const sdk=data(`export{IvyError,requireThat}from'${url('../../../packages/contracts/src/errors.ts')}';export{validateChat}from'${url('../../../packages/contracts/src/chat-validation.ts')}';export{ServiceClient}from'${url('../../../packages/sdk/src/service.ts')}';`);
const host=data(`export async function instanceConfig(){return globalThis[Symbol.for('ivy.test.chat-service-runtime')].config;}
export function configurationPath(){throw Error('Outside isolated service fixture');}
export class HealthFile{constructor(){this.probe=globalThis[Symbol.for('ivy.test.chat-service-runtime')];}write(...args){return this.probe.write(...args);}control(){return this.probe.control();}}`);
const transport=data(`export class WhatsAppRuntime{static async open(){return globalThis[Symbol.for('ivy.test.chat-service-runtime')].transport;}}`);
const registrySdk=data(`export{bundledSchema}from'${url('../../../packages/contracts/src/bundle.ts')}';export{chatInterfaceVersion}from'${url('../../../packages/sdk/src/main-chat.ts')}';`);
const diagnosticsSdk=data(`export{automaticNotificationsAllowed}from'${url('../../../packages/sdk/src/contact-window.ts')}';export{hashJson}from'${url('../../../packages/contracts/src/canonical.ts')}';export{newOperationId}from'${url('../../../packages/sdk/src/client.ts')}';export{requireThat}from'${url('../../../packages/contracts/src/errors.ts')}';`);
registerHooks({resolve(s,c,n){
 if(c.parentURL?.endsWith('/services/chat-bridge/src/main.ts')){
  if(s==='../../../packages/sdk/src/node.js')return{url:sdk,shortCircuit:true};
  if(s==='../../../packages/sdk/src/host.js')return{url:host,shortCircuit:true};
  if(s==='./whatsapp/runtime.js')return{url:transport,shortCircuit:true};
 }
 if(c.parentURL?.endsWith('/services/chat-bridge/src/registry.ts')&&s==='../../../packages/sdk/src/node.js')return{url:registrySdk,shortCircuit:true};
 if(c.parentURL?.endsWith('/services/chat-bridge/src/diagnostic-notifications.ts')&&s==='../../../packages/sdk/src/node.js')return{url:diagnosticsSdk,shortCircuit:true};
 return n(s,c);
}});
const {startChatBridge}=await import('../../../services/chat-bridge/src/main.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {toolDefinitionHash}=await import('../../../packages/contracts/src/tool-definition.ts');
const {canonical}=await import('../../../packages/contracts/src/canonical.ts');
export{starting,now,instructions,noticeInstructions,key,startChatBridge,IvyError,toolDefinitionHash,canonical};
