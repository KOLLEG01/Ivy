import { ChatMain } from './chat-main.js';
import { ChatCollector } from './collector.js';
import { ChatPublisher } from './publisher.js';
import { contention } from './operations.js';
import { ChatCancelWorker } from './cancel-worker.js';

/** Bounded owner loop; durable Hive state, not this instance, owns the execution order. */
export class ChatWorker {
  idle=false;
  runnable=false;
  turnId:string|null=null;
  readonly collector: ChatCollector;
  readonly publisher: ChatPublisher;
  readonly cancellation: ChatCancelWorker;
  constructor(readonly main: ChatMain) { this.collector = new ChatCollector(main); this.publisher = new ChatPublisher(this.collector); this.cancellation = new ChatCancelWorker(this.collector); }
  /** Continue committed progress without a polling delay. Yield after a bounded slice;
   * a native turn that is still running makes no progress and returns to normal polling. */
  async drain(shouldContinue:()=>boolean=()=>true):Promise<void> {
    this.runnable=false;const deadline=performance.now()+50;
    for(let step=0;step<16;step++){
      if(!shouldContinue()){this.runnable=false;return;}
      const before=this.main.store.changeVersion;
      this.runnable=false;
      await this.step();
      this.runnable=!this.idle&&this.main.store.changeVersion!==before;
      if(!this.runnable||performance.now()>=deadline)return;
    }
  }
  async step(): Promise<void> {
    this.idle=false;
    try {
      await this.main.recoverPending();
      const current = await this.main.find(); if (!current?.value.queue.length) {this.idle=true;this.turnId=null;return;}
      if (current.value.publication) return await this.publisher.step();
      // This read only selects a lane. Each lane revalidates the current queue head
      // before acting, so routing does not need a second full admission reconstruction.
      const input = await this.main.store.read('chat-bridge/input', current.value.queue[0]!.inputId);
      this.turnId=input.value.turnId;
      if (input.value.finishedAt && !input.value.turnId) return await this.cancellation.settle();
      if (input.value.turnId) {
        const collection = await this.collector.step(input.pin.objectId);
        if (collection?.value.state === 'complete') await this.publisher.step();
        else if (input.value.cancellation) await this.cancellation.step();
      } else if (input.value.cancellation) await this.cancellation.step();
      else await this.collector.delivery.step();
    } catch (error) { if (!contention(error)) throw error; }
  }
}
