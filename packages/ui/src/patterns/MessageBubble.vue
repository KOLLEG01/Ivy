<script setup lang="ts">
// The user's own message in a conversation: a high-contrast right-aligned bubble with plain text,
// or rich content (Markdown, attachments) passed through the default slot.
defineProps<{ text?: string }>();
</script>
<template>
  <div class="ivy-message-bubble"><slot>{{ text }}</slot></div>
</template>
