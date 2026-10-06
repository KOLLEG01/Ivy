import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../packages/sdk/src/node.js';
import { requireThat, IvyError } from '../../../packages/sdk/src/node.js';
import { managementFrameBytes } from './limits.js';
import type { BrowserNotice } from '../../../packages/sdk/src/node.js';
import { hashJson } from '../../../packages/sdk/src/node.js';

export function nativeBrowserNotice(node: string, event: Pick<Agent.Notification, 'method' | 'params'>): BrowserNotice | null {
  if (event.method !== 'turn/completed') return null;
  const params = event.params as { threadId?: string; turn?: { id?: string; status?: string } };
  if (!params?.threadId || !params.turn?.id || params.turn.status === 'interrupted') return null;
  return { title: params.turn.status === 'failed' ? 'Ivy · Agent needs attention' : 'Ivy · Agent finished',
    body: 'Open the conversation to review the result.', tag: hashJson([node, params.threadId, params.turn.id]),
    target: { uiId: 'agent-ui', fragment: '#/task?' + new URLSearchParams({ node, id: params.threadId, turn: params.turn.id }) } };
}
export function inputBrowserNotice(input: Pick<Agent.PendingInput, 'threadId' | 'identity' | 'state' | 'method'>): BrowserNotice | null {
  if (!input.threadId || input.state !== 'pending' || input.method === 'currentTime/read') return null;
  return { title: 'Ivy · Input needed', body: 'An agent is waiting for your response.', tag: hashJson(input.identity),
    target: { uiId: 'agent-ui', fragment: '#/task?' + new URLSearchParams({ node: input.identity.serviceNodeId, id: input.threadId }) } };
}

/** Transient model events live only in bounded memory. Native turn history owns recovery. */
export class NativeNotifications {
  private sequence:number;private bytes=0;
  private events:Array<{value:Agent.Notification;bytes:number;charge:number}>=[];
  constructor(readonly owner:{serviceNodeId:string;nativeVersion:string},readonly range:{start:number;end:number},readonly maximumBytes:number){this.sequence=range.start;}
  observe(epoch:string,method:string,params:Wire.Json):Agent.Notification{
    requireThat(this.sequence<this.range.end,'native_notification_capacity','Notification sequence reservation exhausted.');
    const value:Agent.Notification={sequence:++this.sequence,serviceNodeId:this.owner.serviceNodeId,nativeVersion:this.owner.nativeVersion,epoch,method,params,observedAt:new Date().toISOString()};
    const text=JSON.stringify(value),bytes=Buffer.byteLength(text),charge=bytes+Buffer.byteLength(JSON.stringify(text));
    if(bytes<=this.maximumBytes){this.events.push({value,bytes,charge});this.bytes+=bytes;}
    while(this.bytes>this.maximumBytes||this.events.length>10000)this.bytes-=this.events.shift()!.bytes;
    return value;
  }
  page(query:Agent.NotificationQuery,epoch:string|null):Agent.NotificationPage{
    validateAgent('NotificationQuery',query);
    const first=this.events[0]?.value.sequence??this.sequence+1,items:Agent.Notification[]=[];
    let last=query.afterSequence,bytes=0,transport=0,hasMore=false,gap=last<first-1||last>this.sequence;
    for(const {value,bytes:size,charge} of this.events){
      if(value.sequence<=query.afterSequence)continue;
      if(items.length===(query.limit??100)||items.length>0&&(bytes+size>9*1024*1024||transport+charge>managementFrameBytes-1024*1024)){hasMore=true;break;}
      if(charge>managementFrameBytes-1024*1024)throw new IvyError('native_notification_too_large','Notification cannot fit a transport-safe page.');
      if(value.sequence!==last+1)gap=true;items.push(structuredClone(value));last=value.sequence;bytes+=size;transport+=charge;
    }
    if(!hasMore&&last!==this.sequence)gap=true;
    const page={epoch,firstAvailableSequence:first,throughSequence:hasMore?last:this.sequence,gap,hasMore,items};
    return page;
  }
}
