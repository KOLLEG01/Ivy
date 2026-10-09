<script setup lang="ts">
// The user's own message in a conversation: plain prose in a compact right-aligned bubble.
defineProps<{ text: string }>();
</script>
<template>
  <div class="ivy-message-bubble">{{ text }}</div>
</template>
