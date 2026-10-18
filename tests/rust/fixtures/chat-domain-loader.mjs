import './chat-store-loader.mjs';
import {registerHooks} from 'node:module';
const exported={
 '../../../packages/contracts/src/canonical.ts':['canonical','digest','hashJson'],
 '../../../packages/contracts/src/errors.ts':['IvyError','requireThat'],
 '../../../packages/contracts/src/chat-validation.ts':['validateChat'],
 '../../../packages/contracts/src/agent-validation.ts':['validateAgent'],
 '../../../packages/contracts/src/checked-native-contract.ts':['checkedNativeContract'],
 '../../../packages/contracts/src/operation-id.ts':['parseOperationId'],
 '../../../packages/contracts/src/limits.ts':['managementFrameBytes'],
 '../../../packages/contracts/src/bundle.ts':['bundledSchema'],
 '../../../packages/sdk/src/receipt-archive.ts':['hasUnresolvedOutcome'],
 '../../../packages/sdk/src/main-chat.ts':['mainNotification'],
};
const sdk='data:text/javascript;base64,'+Buffer.from(Object.entries(exported).map(([path,names])=>`export {${names.join(',')}} from '${new URL(path,import.meta.url)}';`).join('\n')).toString('base64');
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&context.parentURL&&new URL(specifier,context.parentURL).href===new URL('../../../packages/sdk/src/node.js',import.meta.url).href)return{url:sdk,shortCircuit:true};
 return next(specifier,context);
}});
