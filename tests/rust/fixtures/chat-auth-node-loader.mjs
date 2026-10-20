// Resolve this test artifact's fixed library imports through the repository's
// installed dependency graph, including when its worker is in a temporary root.
import {registerHooks} from 'node:module';
const urls=new Map(['baileys','baileys/lib/Signal/libsignal.js'].map(name=>[name,import.meta.resolve(name)]));
registerHooks({resolve(name,context,next){return urls.has(name)?{url:urls.get(name),shortCircuit:true}:next(name,context);}});
