import type { Wire } from '../../packages/sdk/src/node.js';
import protocol from './phone-protocol.json' with { type: 'json' };
import { phoneServiceSchema } from '../../tools/operations/phone-contracts.js';

/** Public discovery input for other components. The Rust registry test verifies
 * these descriptors against the actual service; this fixture has no call behavior. */
export function phoneRegistry(): Wire.RegistrySync {
  const registry = structuredClone(protocol);
  return {
    ...registry,
    contracts: registry.contracts.map(contract=>({...contract,jsonSchema:phoneServiceSchema(contract.jsonSchema)})),
    namespaces: registry.namespaces.map(namespace => ({
      ...namespace,
      tools: namespace.tools.map(tool => ({
        ...tool,
        inputSchema: phoneServiceSchema(tool.inputSchema),
        outputSchema: phoneServiceSchema(tool.outputSchema),
      })),
    })),
  } as Wire.RegistrySync;
}

export { validatePhoneService } from '../../tools/operations/phone-contracts.js';
