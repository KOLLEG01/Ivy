import {existsSync,readFileSync,readdirSync,writeFileSync} from 'node:fs';
import {resolve,relative} from 'node:path';
import {fileURLToPath} from 'node:url';

// Partial source checkouts contain only the service crates delivered so far.
// Materialize their workspace membership; never synthesize service implementations.
const root=fileURLToPath(new URL('../../',import.meta.url));
const members=['packages/contracts/rust','packages/store','packages/sdk/rust','packages/host-runtime/rust'];
if(existsSync(resolve(root,'packages/cli/rust/Cargo.toml')))members.push('packages/cli/rust');
for(const entry of readdirSync(resolve(root,'services'),{withFileTypes:true})){
  if(!entry.isDirectory())continue;
  const path=resolve(root,'services',entry.name,'rust');
  if(existsSync(resolve(path,'Cargo.toml')))members.push(relative(root,path).replaceAll('\\','/'));
}
const path=resolve(root,'Cargo.toml'),original=readFileSync(path,'utf8');
const updated=original.replace(/^members = \[[^\]]*\]/m,`members = ${JSON.stringify(members)}`);
if(updated!==original){if(process.argv.includes('--check'))throw Error('Rust workspace membership differs; run register-rust-members.mjs.');writeFileSync(path,updated);}
