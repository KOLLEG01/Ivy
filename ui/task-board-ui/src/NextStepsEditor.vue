<script setup lang="ts">
import { Button, Field, FieldDescription, FieldGroup, Input, Label, OptionSelect, Textarea } from '@ivy/ui';
import { Plus, X } from '@lucide/vue';
import { toRaw } from 'vue';
import type { TaskBoard } from './runtime';
import { tr } from './runtime';

const props = defineProps<{ modelValue: TaskBoard.NextStep[]; disabled?: boolean;
  questions?: TaskBoard.Comment[]; tasks?: { value: string; label: string }[] }>();
const emit = defineEmits<{ 'update:modelValue': [value: TaskBoard.NextStep[]] }>();
const change = (edit: (steps: TaskBoard.NextStep[]) => void) => { const steps = structuredClone(toRaw(props.modelValue)); edit(steps); emit('update:modelValue', steps); };
const valueOf = (event: Event) => (event.target as HTMLInputElement).value;
const changeTime = (stepIndex: number, conditionIndex: number, event: Event) => {
  const value = valueOf(event), timestamp = Date.parse(value);
  if (Number.isFinite(timestamp)) updateCondition(stepIndex, conditionIndex, { at: new Date(timestamp).toISOString() });
};
const localDate = (value: string) => { if (!value) return ''; const date = new Date(value); return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16); };
const replaceCondition = (stepIndex: number, conditionIndex: number, kind: string) => change(steps => {
  const condition: TaskBoard.Condition = kind === 'time' ? { kind, at: new Date(Date.now() + 3600_000).toISOString() } :
    kind === 'task' ? { kind, taskId: props.tasks?.[0]?.value ?? '', until: 'done' } :
    kind === 'delivery' ? { kind, taskId: props.tasks?.[0]?.value ?? '', key: '' } : { kind: 'user', questionId: props.questions?.[0]?.commentId ?? '' };
  steps[stepIndex]!.requires[conditionIndex] = condition;
});
const addStep = () => change(steps => steps.push({ id: crypto.randomUUID(), scope: '', requires: [] }));
const updateCondition = (stepIndex: number, conditionIndex: number, values: Record<string, string>) => change(steps => {
  Object.assign(steps[stepIndex]!.requires[conditionIndex]!, values);
});
</script>

<template>
  <FieldGroup>
    <FieldDescription>{{ tr('Jeder Schritt kann unabhängig starten. Innerhalb eines Schritts müssen alle Voraussetzungen erfüllt sein.', 'Each step can start independently. Every condition within a step must be met.') }}</FieldDescription>
    <div v-for="(step, stepIndex) in modelValue" :key="step.id" class="space-y-3 rounded-lg border p-3">
      <Field>
        <div class="flex items-center justify-between gap-2">
          <Label :for="'step-scope-' + step.id">{{ tr('Nächster Schritt', 'Next step') }}</Label>
          <Button type="button" variant="ghost" size="icon-sm" :disabled="disabled" :aria-label="tr('Schritt entfernen', 'Remove step')" @click="change(steps => steps.splice(stepIndex, 1))"><X aria-hidden="true" /></Button>
        </div>
        <Textarea :id="'step-scope-' + step.id" :model-value="step.scope" :disabled="disabled" required :maxlength="16384" @update:model-value="change(steps => { steps[stepIndex]!.scope = String($event); })" />
      </Field>
      <FieldDescription v-if="!step.requires.length">{{ tr('Kann sofort beginnen.', 'Can begin immediately.') }}</FieldDescription>
      <div v-for="(condition, conditionIndex) in step.requires" :key="conditionIndex" class="flex flex-wrap items-end gap-2">
        <Field class="min-w-36 flex-1">
          <Label>{{ tr('Voraussetzung', 'Condition') }}</Label>
          <OptionSelect :model-value="condition.kind" :disabled="disabled" @update:model-value="replaceCondition(stepIndex, conditionIndex, String($event))">
            <option value="time">{{ tr('Termin', 'Appointment') }}</option><option value="task">{{ tr('Anderes Ticket', 'Another ticket') }}</option>
            <option value="delivery">{{ tr('Benanntes Ergebnis', 'Named delivery') }}</option><option value="user" :disabled="!questions?.length">{{ tr('Eigene Nutzerfrage', 'Own user question') }}</option>
          </OptionSelect>
        </Field>
        <Field v-if="condition.kind === 'time'" class="min-w-44 flex-1"><Label>{{ tr('Zeitpunkt', 'Time') }}</Label><Input type="datetime-local" :model-value="localDate(condition.at)" :disabled="disabled" required @change="changeTime(stepIndex, conditionIndex, $event)" /></Field>
        <template v-else-if="condition.kind === 'task' || condition.kind === 'delivery'">
          <Field class="min-w-44 flex-1"><Label>Ticket</Label><OptionSelect :model-value="condition.taskId" :disabled="disabled" @update:model-value="updateCondition(stepIndex, conditionIndex, { taskId: String($event) })"><option v-for="task in tasks" :key="task.value" :value="task.value">{{ task.label }}</option></OptionSelect></Field>
          <Field v-if="condition.kind === 'task'" class="min-w-36 flex-1"><Label>{{ tr('Erwarteter Zustand', 'Expected state') }}</Label><OptionSelect :model-value="condition.until" :disabled="disabled" @update:model-value="updateCondition(stepIndex, conditionIndex, { until: String($event) })"><option value="idle">{{ tr('Bereit zur Prüfung', 'Ready for review') }}</option><option value="done">{{ tr('Abgeschlossen', 'Done') }}</option></OptionSelect></Field>
          <Field v-else class="min-w-36 flex-1"><Label>{{ tr('Ergebnisname', 'Delivery name') }}</Label><Input :model-value="condition.key" :disabled="disabled" required @update:model-value="updateCondition(stepIndex, conditionIndex, { key: String($event) })" /></Field>
        </template>
        <Field v-else class="min-w-44 flex-1"><Label>{{ tr('Frage dieses Tickets', 'Question in this ticket') }}</Label><OptionSelect :model-value="condition.questionId" :disabled="disabled" @update:model-value="updateCondition(stepIndex, conditionIndex, { questionId: String($event) })"><option v-for="question in questions" :key="question.commentId" :value="question.commentId">{{ question.body.slice(0, 120) }}</option></OptionSelect></Field>
        <Button type="button" variant="ghost" size="icon" :disabled="disabled" :aria-label="tr('Voraussetzung entfernen', 'Remove condition')" @click="change(steps => steps[stepIndex]!.requires.splice(conditionIndex, 1))"><X aria-hidden="true" /></Button>
      </div>
      <Button type="button" variant="outline" size="sm" :disabled="disabled" @click="change(steps => steps[stepIndex]!.requires.push({ kind: 'time', at: new Date(Date.now() + 3600_000).toISOString() }))"><Plus aria-hidden="true" />{{ tr('Voraussetzung ergänzen', 'Add condition') }}</Button>
    </div>
    <Button type="button" variant="outline" :disabled="disabled" @click="addStep"><Plus aria-hidden="true" />{{ tr('Unabhängigen Schritt ergänzen', 'Add independent step') }}</Button>
  </FieldGroup>
</template>
