<template>
  <media-player :title="title" :src="src">
    <media-provider />
  </media-player>
</template>

<script lang="ts" setup>
withDefaults(
  defineProps<{
    title?: string;
    src?: string;
  }>(),
  {
    title: undefined,
    src: undefined,
  }
);
</script>
