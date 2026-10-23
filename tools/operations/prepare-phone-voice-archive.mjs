import { jsonFile } from '../../dist/packages/host-runtime/src/config.js';
import { preparePhoneVoiceArchive } from '../../dist/tools/operations/phone-voice-archive.js';
if (process.argv.length !== 3) throw new Error('Usage: node tools/operations/prepare-phone-voice-archive.mjs <explicit-setup-request.json>');
const result = await preparePhoneVoiceArchive(await jsonFile(process.argv[2], 65536));
process.stdout.write(JSON.stringify(result, null, 2) + '\n');
