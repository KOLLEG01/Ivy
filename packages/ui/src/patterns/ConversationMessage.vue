<script setup lang="ts">
import type { Component } from "vue";
import { Bot, SquareKanban } from "@lucide/vue";
import MessageBubble from "./MessageBubble.vue";

// One message in a conversation timeline. Your own messages are high-contrast bubbles on the right;
// replies are raised cards on the left and messages from another source use a dashed outline.
// The sender is named above the bubble, never inside it; the avatar shows from tablet width up.
defineProps<{ variant: "user" | "reply" | "external"; label?: string; icon?: Component }>();
</script>
<template>
  <MessageBubble v-if="variant === 'user'" data-slot="conversation-message"><slot /></MessageBubble>
  <div v-else data-slot="conversation-message" class="ivy-chat-message" :data-variant="variant">
    <div class="ivy-chat-label"><slot name="label">{{ label }}</slot></div>
    <div class="ivy-chat-row">
      <span class="ivy-chat-avatar" aria-hidden="true"
        ><component :is="icon ?? (variant === 'external' ? SquareKanban : Bot)"
      /></span>
      <div class="ivy-chat-card"><slot /></div>
    </div>
  </div>
</template>
