import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes, createRequire } from 'node:module';
const require = createRequire(process.env.IVY_TEST_PACKAGE_JSON || import.meta.url);
const { z } = require('zod');
const source = readFileSync(new URL('../../services/data-collector/src/schema.ts', import.meta.url), 'utf8');
const js = stripTypeScriptTypes(source, { mode: 'transform' }).replace(/import \{ z \} from "zod";/, 'const z = globalThis.__collectorZod;');
globalThis.__collectorZod = z;
const schemas = await import('data:text/javascript;base64,' + Buffer.from(js).toString('base64'));
delete globalThis.__collectorZod;
const names = ['retentionSchema','mcpSchema','taskSchema','settingsSchema','outputSchema','runtimeReadSchema','runtimeUpdateSchema','pinSchema','scriptDocument','definitionDocument','assignmentDocument','observationDocument','definitionReadSchema','definitionUpdateSchema','readSchema','updateSchema'];
function assets() {
  const documents = {};
  for (const name of names) documents[name] = z.toJSONSchema(schemas[name], {
    io: 'input',
    override({zodSchema,jsonSchema}) {
      // JSON has no coercions/defaults. Preserve it without recursive schema evaluation.
      if (zodSchema._zod.def.type === 'lazy') {
        for (const key of Object.keys(jsonSchema)) delete jsonSchema[key];
        jsonSchema['x-collector-json'] = true;
      }
      if (zodSchema === schemas.mcpSchema) jsonSchema['x-collector-distinct-tools'] = true;
      if (zodSchema === schemas.taskSchema) jsonSchema['x-collector-input-auth'] = true;
    },
  });
  // Each standalone Zod document owns its references. Give it a stable place in
  // the single trusted Rust bundle, without changing the public registry.
  for (const [name, document] of Object.entries(documents)) {
    const pending = [document];
    while (pending.length) {
      const value = pending.pop();
      if (!value || typeof value !== 'object') continue;
      if (typeof value.$ref === 'string' && value.$ref.startsWith('#'))
        value.$ref = '#/$defs/' + name + value.$ref.slice(1);
      pending.push(...Object.values(value));
    }
  }
  return { registry: schemas.registry(), documents: {$defs:documents} };
}
if (process.argv[2] === 'assets') {
  process.stdout.write(JSON.stringify(assets()));
} else if (process.argv[2] === 'cases') {
  const task = {id:'fixture',name:'Fixture',enabled:false,intervalSeconds:60,timeoutSeconds:5,memoryMb:128,script:'export default async () => ({data:null});',dependencies:{},config:{},secretNames:[],retention:schemas.defaultRetention};
  const mcp = {serviceNodeId:'host.agent',cwd:'relative',server:'fixture',tools:['one','two']};
  const pin = {objectId:'object',revision:1};
  const definitionTask = {...task}; delete definitionTask.script; delete definitionTask.enabled;
  const samples = [
    ['retentionSchema',schemas.defaultRetention],['mcpSchema',mcp],['taskSchema',task],['taskSchema',{...task,mcp}],
    ['settingsSchema',{}],['settingsSchema',{ingress:{port:8080,routes:{'/input':'fixture'}}}],
    ['outputSchema',{data:null}],['outputSchema',{data:{x:1},state:false,events:[{name:'x.y',payload:[]}]}],
    ['pinSchema',pin],['scriptDocument',{schemaVersion:1,script:task.script}],
    ['definitionDocument',{schemaVersion:1,homeServiceNodeId:'host.collector',task:definitionTask,script:pin}],
    ['assignmentDocument',{schemaVersion:1,jobId:'job',definitionRevision:1,serviceNodeId:'host.collector',enabled:false,rootObjectId:'root',resultObjectId:null}],
    ['observationDocument',{schemaVersion:1,jobId:'job',serviceNodeId:'host.collector',assignmentRevision:1,definitionRevision:1,accepted:true,everAccepted:true,quiesced:false,error:null,observedAt:'opaque'}],
  ];
  for (const value of [{view:'tasks'},{view:'task',id:'fixture'},{view:'result',id:'fixture',revision:1},{view:'history',id:'fixture',cursor:''},{view:'operation',operationId:'op'},
    {view:'definitions'},{view:'definition',jobId:'job'},{view:'definitionHistory',jobId:'job',cursor:''},{view:'script',objectId:'object'},{view:'handoff',jobId:'job',expectedRevision:1}]) samples.push(['readSchema',value]);
  for (const value of [
    {action:'save',operationId:'op',expectedRevision:0,task},
    ...['enable','disable','delete'].map(action=>({action,operationId:'op',id:'fixture',expectedRevision:1})),
    {action:'run',operationId:'op',id:'fixture',input:null},
    {action:'saveScript',operationId:'op',expectedRevision:0,script:task.script},
    {action:'activate',operationId:'op',jobId:'job',definitionRevision:1,expectedRevision:0,enabled:true},
    {action:'recover',operationId:'op',jobId:'job',expectedRevision:1,confirmedStopped:true},
    {action:'import',operationId:'op'},
    ...['freezeHandoff','prepareHandoff'].map(action=>({action,operationId:'op',jobId:'job',expectedRevision:1})),
  ]) samples.push(['updateSchema',value]);
  samples.push(['runtimeUpdateSchema',{action:'save',operationId:'op',expectedRevision:0,task}]);
  const rows = [];
  function add(name,value) {
    const result=schemas[name].safeParse(value);
    rows.push({name,input:value,accepted:result.success,...(result.success?{value:result.data}:{})});
  }
  for (const [name,value] of samples) {
    add(name,value); add(name,{...value,extra:true});
    for (const key of Object.keys(value)) {
      const missing={...value}; delete missing[key]; add(name,missing);
      for (const change of [null,{},[],false,0,-1,1.5,9007199254740992,'','\ud800','😀']) add(name,{...value,[key]:change});
    }
  }
  for (const name of ['settingsSchema','taskSchema','outputSchema','readSchema','updateSchema'])
    for (const value of [null,false,[],0,'']) add(name,value);
  for (const name of ['x'.repeat(200),'😀'.repeat(100),'😀'.repeat(101),'😀'.repeat(200),'😀'.repeat(201),'x'.repeat(201),'\ud800'.repeat(200)]) add('taskSchema',{...task,name});
  for (const dependencies of [{'@scope/package':'1.2.3-pre.1'},{package:'^1.2.3'},{'Bad':'1.2.3'},{'a':'1.2.3+build'},JSON.parse('{"__proto__":"1.2.3"}')]) add('taskSchema',{...task,dependencies});
  for (const config of [JSON.parse('{"__proto__":1,"constructor":2,"x":{"__proto__":3,"k":[null,false]}}'),{'\ud800':'\udfff'}]) add('taskSchema',{...task,config});
  for (const input of [{inputSecretName:'TOKEN',allowUnauthenticatedInput:true},{inputSecretName:'TOKEN',allowUnauthenticatedInput:false},{inputSecretName:null,allowUnauthenticatedInput:true},{secretNames:['TOKEN','TOKEN']},{secretNames:['not-ok']},{mcp:{...mcp,tools:['one','one']}}]) {
    add('taskSchema',{...task,...input}); add('updateSchema',{action:'save',operationId:'op',expectedRevision:0,task:{...task,...input}});
    add('definitionDocument',{schemaVersion:1,homeServiceNodeId:'host.collector',script:pin,task:{...definitionTask,...input}});
  }
  for (const secrets of [{'TOKEN':'value'},{'bad-name':'value'},JSON.parse('{"__proto__":"x"}')]) add('settingsSchema',{secrets});
  add('outputSchema',{data:JSON.parse('{"__proto__":1,"x":2}')});
  add('definitionDocument',{schemaVersion:1,homeServiceNodeId:'host.collector',script:pin,task:{...definitionTask,enabled:true,script:'extra',other:true}});
  process.stdout.write(JSON.stringify({assets:assets(),rows}));
} else {
  const input = JSON.parse(readFileSync(0,'utf8'));
  const rows = input.map(({name,value}) => {
    const result = schemas[name].safeParse(value);
    return result.success ? {accepted:true,value:result.data} : {accepted:false};
  });
  process.stdout.write(JSON.stringify(rows));
}
