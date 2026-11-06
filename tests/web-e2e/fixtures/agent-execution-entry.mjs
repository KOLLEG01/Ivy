import { createApp } from 'vue';
import AgentUi from '../../../ui/agent-ui/src/AgentUi.vue';
import { live } from '../../../ui/agent-ui/src/runtime';
import '@ivy/ui/styles.css';
if (!location.hash) location.hash = '#/hosts';
createApp(AgentUi).use(live).mount('#ui');
