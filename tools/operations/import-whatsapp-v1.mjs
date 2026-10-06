import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {dirname,resolve} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {parseArgs} from 'node:util';
import {WhatsAppJournal,identity} from '../../dist/services/chat-bridge/src/whatsapp/journal.js';
const {values}=parseArgs({options:Object.fromEntries(['source-config','source-db','destination-config','data-root','principal','channel'].map(k=>[k,{type:'string'}]))});
for(const k of ['source-config','source-db','destination-config','data-root','principal','channel'])if(!values[k])throw Error('Missing '+k);
const source=JSON.parse(await readFile(resolve(values['source-config']),'utf8'));
if(source.IvyCore?.chat?.enabled!==false)throw Error('Stop/disable the previous IvyChat writer before importing its account.');
const account=source.IvyCore.chat.whatsapp.accountId;
const db=new DatabaseSync(resolve(values['source-db']),{readOnly:true});
const creds=db.prepare('SELECT value_json FROM whatsapp_auth_creds WHERE account=?').get(account);
if(!creds)throw Error('No saved WhatsApp account');
const parsed=JSON.parse(creds.value_json),bot=/^(\d+)(?::\d+)?@/.exec(parsed.me?.id??'')?.[1];
if(!bot)throw Error('Saved account has no verifiable phone identity');
const allowed=source.IvyCore.chat.allowedSenders;
if(!Array.isArray(allowed)||allowed.length!==1||!allowed[0].phoneNumber||allowed[0].account!==account)throw Error('This import needs exactly one configured phone sender on the selected account.');
const normalize=p=>{let n=p.replace(/[\s().-]/g,'').replace(/^00/,'+');if(!n.startsWith('+'))n='+'+n;if(!/^\+[1-9]\d{6,14}$/.test(n))throw Error('Invalid original phone identity');return n;};
const journal=new WhatsAppJournal(resolve(values['data-root'],'whatsapp.sqlite'));
try{
  const marker=identity(resolve(values['source-db']),account);
  if(journal.get('v1-import')===marker){console.log(JSON.stringify({alreadyImported:true}));process.exitCode=0;}
  else{
    if(journal.auth('creds','default')||journal.get('identity'))throw Error('Destination already has an account; never overwrite live authentication.');
    const config={accountId:account,botPhoneNumber:'+'+bot,since:new Date().toISOString(),heartbeatMs:120000,
      senders:[{phoneNumber:normalize(allowed[0].phoneNumber),principalId:values.principal,channelId:values.channel}]};
    if(source.OpenAI?.apiKey){
      config.transcription={apiKey:source.OpenAI.apiKey,baseUrl:source.OpenAI.baseUrl??'https://api.openai.com/v1',model:source.OpenAI.transcription?.model??'gpt-4o-mini-transcribe',
        ...(source.OpenAI.transcription?.language?{language:source.OpenAI.transcription.language}:{})};
    }
    await mkdir(dirname(resolve(values['destination-config'])),{recursive:true,mode:0o700});
    await writeFile(resolve(values['destination-config']),JSON.stringify(config,null,2)+'\n',{flag:'wx',mode:0o600});
    journal.transaction(()=>{journal.saveAuth('creds','default',creds.value_json);
      for(const row of db.prepare('SELECT category,key_id,value_json FROM whatsapp_auth_keys WHERE account=?').all(account))journal.saveAuth(row.category,row.key_id,row.value_json);
      journal.set('v1-import',marker);
    });
    console.log(JSON.stringify({imported:true,senders:1,registered:!!parsed.registered,transcriptionConfigured:!!config.transcription}));
  }
}finally{journal.close();db.close();}
