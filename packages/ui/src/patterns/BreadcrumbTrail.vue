<script setup lang="ts">
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbSeparator,
} from "../components/breadcrumb";
defineProps<{
  label: string;
  root: { label: string; href: string };
  items: Array<{ id: string; label: string; href: string }>;
}>();
</script>
<template>
  <Breadcrumb :aria-label="label" class="min-w-0">
    <BreadcrumbList class="flex-nowrap overflow-hidden">
      <BreadcrumbItem class="shrink-0">
        <BreadcrumbLink as-child
          ><a :href="root.href">{{ root.label }}</a></BreadcrumbLink
        >
      </BreadcrumbItem>
      <template v-for="(item, index) in items" :key="item.id">
        <BreadcrumbSeparator />
        <BreadcrumbItem class="min-w-0">
          <BreadcrumbLink as-child
            ><a
              :href="item.href"
              class="block max-w-48 truncate"
              :class="{ 'text-foreground': index === items.length - 1 }"
              :title="item.label"
              :aria-current="index === items.length - 1 ? 'page' : undefined"
              >{{ item.label }}</a
            ></BreadcrumbLink
          >
        </BreadcrumbItem>
      </template>
    </BreadcrumbList>
  </Breadcrumb>
</template>
