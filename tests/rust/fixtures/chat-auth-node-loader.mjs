// Resolve this test artifact's fixed library imports through the repository's
// installed dependency graph, including when its worker is in a temporary root.
import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
const urls=new Map(['baileys','baileys/lib/Signal/libsignal.js'].map(name=>[name,import.meta.resolve(name)]));
registerHooks({resolve(name,context,next){
  if(urls.has(name))return{url:urls.get(name),shortCircuit:true};
  if(name.startsWith('.')&&name.endsWith('.js')&&context.parentURL?.startsWith('file:')){
    const source=new URL(name.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(source))return{url:source.href,shortCircuit:true};
  }
  return next(name,context);
}});
