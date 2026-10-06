import { serviceTools } from './client.js';
import type { RpcClient, RequestOptions } from './client.js';
import { NativeContract } from '../../contracts/src/native-contract.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Agent, Wire } from '../../contracts/src/generated.js';
import { validateNativeStatus } from './native-evidence.js';

export type NativeTarget = Pick<Agent.Status, 'serviceNodeId' | 'hostId' | 'nativeVersion' | 'nativeExecutableHash' | 'catalogHash'>;
export interface NativeContractSnapshot { target: NativeTarget; epoch: string; contract: NativeContract }

/** Read one selected native owner without a latest-version lookup or permission to replay calls. */
export async function readNativeContract(client: RpcClient, target: NativeTarget, options?: RequestOptions): Promise<NativeContractSnapshot> {
  const expected = structuredClone(target);
  const management = async (name: string): Promise<Wire.Json> => {
    options?.signal?.throwIfAborted();
    return serviceTools(client, expected.serviceNodeId, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).call('agent.' + name, {}, undefined, options);
  };
  const status = async (): Promise<Agent.Status> => {
    const value = await management('status'); validateNativeStatus(value, expected); return value;
  };
  const before = await status(), value = await management('catalog'); validateAgent('Catalog', value);
  const contract = new NativeContract(value as Agent.Catalog);
  requireThat(contract.catalogHash === expected.catalogHash && contract.catalog.version === expected.nativeVersion && contract.catalog.nativeExecutableHash === expected.nativeExecutableHash,
    'native_catalog_mismatch', 'The provider catalog differs from the retained native version, executable or catalog hash.');
  const after = await status();
  requireThat(before.epoch === after.epoch, 'native_epoch_changed', 'The native owner restarted during contract discovery.');
  return { target: expected, epoch: before.epoch!, contract };
}
