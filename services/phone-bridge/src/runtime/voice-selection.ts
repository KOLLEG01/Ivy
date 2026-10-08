import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';

export interface PhoneVoiceSelection {
  model: string;
  reasoningEffort: string;
}

export interface PhoneVoiceModel {
  model: string;
  displayName: string;
  description: string;
  defaultReasoningEffort: string;
  supportedReasoningEfforts: { reasoningEffort: string; description: string }[];
  isDefault: boolean;
}

export function validatePhoneVoiceSelection(selection: PhoneVoiceSelection, models: readonly PhoneVoiceModel[]): void {
  const model = models.find(item => item.model === selection.model);
  requireThat(model?.supportedReasoningEfforts.some(item => item.reasoningEffort === selection.reasoningEffort),
    'phone_voice_selection_invalid', 'Select a model and reasoningEffort advertised by phone_bridge_status.voiceModels.');
}

export const defaultPhoneVoiceSelection: PhoneVoiceSelection = Object.freeze({
  model: 'gpt-6-sol',
  reasoningEffort: 'high',
});

export function selectedPhoneVoiceModel(model: string | null | undefined, reasoningEffort: string | null | undefined): PhoneVoiceSelection {
  // Stable keypad shortcuts; the native model catalog validates the resulting selection.
  const models = { luna: 'gpt-6-luna', sol: 'gpt-6-sol', astra: 'gpt-6-astra' } as const;
  if (!model || !Object.hasOwn(models, model) || !reasoningEffort)
    throw new IvyError('phone_voice_selection_invalid', 'Voice model and reasoning selection is unsupported.');
  return { model: models[model as keyof typeof models], reasoningEffort: reasoningEffort as PhoneVoiceSelection['reasoningEffort'] };
}
