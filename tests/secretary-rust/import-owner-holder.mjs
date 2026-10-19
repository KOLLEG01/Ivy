import {load,rootId} from './inbox-fixtures.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const store=new SecretaryStore({request(){throw Error('No external requests');}},rootId,process.argv[2]);
store.db.prepare('SELECT COUNT(*) FROM documents').get();
process.stdout.write('ready\n');
process.stdin.once('data',()=>{store.close();process.stdout.write('closed\n');process.stdin.pause();});
