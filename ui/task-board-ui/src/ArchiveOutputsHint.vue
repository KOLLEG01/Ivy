<script setup lang="ts">
import { DropdownMenuLabel } from "@ivy/ui";
import { tr } from "./runtime";
</script>

<template>
  <DropdownMenuLabel class="max-w-64 whitespace-normal font-normal text-muted-foreground">
    {{ tr("Temporäre Arbeitsdateien werden endgültig gelöscht. Anhänge bleiben erhalten.", "Temporary working files are permanently deleted. Attachments are preserved.") }}
  </DropdownMenuLabel>
</template>
