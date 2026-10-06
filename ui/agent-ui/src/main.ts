import { createApp } from 'vue';
import '@ivy/ui/styles.css';
import AgentUi from './AgentUi.vue';
import { live } from './runtime';
createApp(AgentUi).use(live).mount('#ui');
