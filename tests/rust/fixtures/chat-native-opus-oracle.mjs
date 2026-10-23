// Compare native decoding with the original codec, entirely on local bytes.
import {readFileSync,writeFileSync} from 'node:fs';
import decodeOpus from '@audio/decode-opus';
import {encodeMonoPcm16Wav} from '../../../services/chat-bridge/src/whatsapp/audio-processing.ts';
const [input,output]=process.argv.slice(2);
writeFileSync(output,encodeMonoPcm16Wav(await decodeOpus(readFileSync(input))));
