// Private auth IPC. A frame is bounded; an accepted logical row/batch is not.
import {TextDecoder} from 'node:util';
const FRAME_BYTES = 65536, CHUNK_BYTES = 45 * 1024;
const utf8 = new TextDecoder('utf-8', {fatal:true});

export function authFrames(input, output, generation) {
  let sent = 0, received = 0, writing = Promise.resolve();
  async function write(bytes) {
    await new Promise((resolve, reject) => output.write(bytes, error => error ? reject(error) : resolve()));
  }
  return {
    send(request, value) {
      // Only one writer and one logical callback are in flight. Stream writes
      // wait for their completion, rather than accumulating unbounded frames.
      const work = writing.then(async () => {
        const bytes = Buffer.from(JSON.stringify(value)), message = String(++sent);
        for (let offset = 0, part = 0; offset < bytes.length; offset += CHUNK_BYTES, part++) {
          const frame = Buffer.from(JSON.stringify({v:1,g:generation,r:request,m:message,p:part,e:offset+CHUNK_BYTES>=bytes.length,b:bytes.subarray(offset,offset+CHUNK_BYTES).toString('base64')}));
          if (frame.length + 4 > FRAME_BYTES) throw Error('Invalid auth frame.');
          const header = Buffer.alloc(4); header.writeUInt32BE(frame.length);
          await write(Buffer.concat([header,frame]));
        }
      });
      writing = work;
      return work;
    },
    async *messages() {
      // Read precisely one bounded frame at a time. The stream controls its
      // own finite buffer; only the one current logical message is assembled.
      const source = input[Symbol.asyncIterator]();
      let buffered = Buffer.alloc(0), ended = false;
      async function read(count) {
        const result = Buffer.alloc(count); let offset = 0;
        while (offset < count) {
          if (!buffered.length) {
            const item = await source.next();
            if (item.done) { ended = true; if (!offset) return null; throw Error('Partial auth frame.'); }
            buffered = item.value;
          }
          const length = Math.min(count-offset, buffered.length);
          buffered.copy(result,offset,0,length); buffered=buffered.subarray(length); offset+=length;
        }
        return result;
      }
      let message = null;
      while (!ended) {
        const header = await read(4);
        if (!header) { if (message) throw Error('Partial auth message.'); return; }
        const length = header.readUInt32BE();
        if (!length || length + 4 > FRAME_BYTES) throw Error('Invalid auth frame.');
        const bytes = await read(length); if (!bytes) throw Error('Partial auth frame.');
        const frame = JSON.parse(utf8.decode(bytes));
        if (frame.v!==1 || frame.g!==generation || typeof frame.r!=='string' || frame.m!==String(received+1) || !Number.isSafeInteger(frame.p) || typeof frame.e!=='boolean' || typeof frame.b!=='string') throw Error('Stale auth frame.');
        if (!message) message={request:frame.r,part:0,chunks:[]};
        if (frame.r!==message.request || frame.p!==message.part++) throw Error('Unordered auth frame.');
        const chunk=Buffer.from(frame.b,'base64');
        if (chunk.length>CHUNK_BYTES || chunk.toString('base64')!==frame.b) throw Error('Invalid auth chunk.');
        message.chunks.push(chunk);
        if (frame.e) {
          const value=JSON.parse(utf8.decode(Buffer.concat(message.chunks))), request=message.request;
          received++; message=null; yield {request,value};
        }
      }
    },
  };
}
