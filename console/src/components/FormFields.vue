<script setup lang="ts">
defineProps<{ fields: { k: string; label: string; type?: 'text' | 'number' | 'bool' | 'select' | 'json' | 'list' | 'datetime'; options?: string[]; hint?: string; disabled?: boolean }[]; model: Record<string, any> }>()
</script>
<template>
  <div class="f" v-for="f in fields" :key="f.k">
    <label v-if="f.type !== 'bool'">{{ f.label }}<span class="dim" v-if="f.hint"> — {{ f.hint }}</span></label>
    <label v-if="f.type === 'bool'"><input type="checkbox" v-model="model[f.k]" :disabled="f.disabled" /> {{ f.label }}<span class="dim" v-if="f.hint"> — {{ f.hint }}</span></label>
    <input v-else-if="f.type === 'number'" class="in" type="number" v-model.number="model[f.k]" :disabled="f.disabled" />
    <select v-else-if="f.type === 'select'" class="sel" v-model="model[f.k]" :disabled="f.disabled"><option v-for="o in f.options" :key="o" :value="o">{{ o }}</option></select>
    <textarea v-else-if="f.type === 'json'" class="in m" v-model="model[f.k]" style="min-height:90px" :disabled="f.disabled"></textarea>
    <input v-else-if="f.type === 'datetime'" class="in" type="datetime-local" v-model="model[f.k]" :disabled="f.disabled" />
    <input v-else class="in" :class="{ m: f.type === 'list' }" v-model="model[f.k]" :disabled="f.disabled" :placeholder="f.type === 'list' ? 'comma-separated' : ''" />
  </div>
</template>
