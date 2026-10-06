import { createApp } from 'vue';
import '@ivy/ui/styles.css';
import ConsoleUi from './ConsoleUi.vue';
import { live } from './runtime';
createApp(ConsoleUi).use(live).mount('#ui');
