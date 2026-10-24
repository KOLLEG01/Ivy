<script setup lang="ts">
import { Alert, AlertTitle, AlertDescription, Button, ContentView, ExpandableContent } from '@ivy/ui';
import NativeInput from '../../../packages/ui-client/src/NativeInput.vue';
import { base, client, tr } from './runtime';
import type { TaskBoard } from './runtime';

defineProps<{ questions: TaskBoard.Comment[]; available: boolean }>();
const emit = defineEmits<{ answer: [commentId: string]; answered: [] }>();
</script>

<template>
  <section v-if="questions.length" data-slot="task-pending-questions" :aria-label="tr('Offene Fragen dieses Tickets', 'Open questions in this ticket')" class="max-h-[25dvh] space-y-2 overflow-y-auto py-1">
    <Alert v-for="question in questions" :key="question.commentId" data-attention="question">
      <AlertTitle>{{ tr('Deine Antwort wird benötigt', 'Your answer is needed') }}</AlertTitle>
      <AlertDescription class="space-y-2">
        <NativeInput v-if="question.nativeInput" :input="question.nativeInput" :epoch="question.nativeInput.identity.epoch" :available="available" :base="base" :client="client" @answered="emit('answered')" />
        <template v-else>
          <ExpandableContent :preview-lines="2" :show-more-label="tr('Frage anzeigen', 'Show question')" :show-less-label="tr('Weniger anzeigen', 'Show less')"><ContentView :text="question.body" media-type="text/markdown" /></ExpandableContent>
          <Button size="sm" variant="outline" :disabled="!available" @click="emit('answer', question.commentId)">{{ tr('Antworten', 'Answer question') }}</Button>
        </template>
      </AlertDescription>
    </Alert>
  </section>
</template>
