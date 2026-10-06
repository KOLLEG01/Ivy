import { IvyError } from '../../../../packages/sdk/src/node.js';

export interface PhoneVoiceSelection {
  model: 'gpt-6-luna' | 'gpt-6-sol' | 'gpt-6-astra';
  reasoningEffort: 'low' | 'medium' | 'high' | 'xhigh' | 'max' | 'ultra';
}

export const defaultPhoneVoiceSelection: PhoneVoiceSelection = Object.freeze({
  model: 'gpt-6-sol',
  reasoningEffort: 'high',
});

export function selectedPhoneVoiceModel(model: string | null | undefined, reasoningEffort: string | null | undefined): PhoneVoiceSelection {
  const models = { luna: 'gpt-6-luna', sol: 'gpt-6-sol', astra: 'gpt-6-astra' } as const;
  const efforts = ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'] as const;
  if (!model || !Object.hasOwn(models, model) || !reasoningEffort || !efforts.includes(reasoningEffort as typeof efforts[number]) ||
      model === 'luna' && reasoningEffort === 'ultra')
    throw new IvyError('phone_voice_selection_invalid', 'Voice model and reasoning selection is unsupported.');
  return { model: models[model as keyof typeof models], reasoningEffort: reasoningEffort as PhoneVoiceSelection['reasoningEffort'] };
}
