import { buildBackend } from './build-backend.mjs';
import { buildWeb } from './build-web.mjs';
import { buildSdk } from './build-sdk.mjs';

await buildBackend();
buildSdk();
await buildWeb();
