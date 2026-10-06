import { parentPort,workerData } from 'node:worker_threads';
import decodeOpus from '@audio/decode-opus';
import { decodedAudioDurationSeconds,encodeMonoPcm16Wav } from './audio-processing.js';
try{
  const decoded=await decodeOpus(workerData as Uint8Array);
  if(decodedAudioDurationSeconds(decoded)>600)throw Error('duration');
  const wav=encodeMonoPcm16Wav(decoded);parentPort!.postMessage({wav});
}catch{parentPort!.postMessage({error:'whatsapp_audio_invalid'});}
