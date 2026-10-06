import { createApp } from 'vue';
import '@ivy/ui/styles.css';
import WikiUi from './WikiUi.vue';
import { live } from './runtime';
createApp(WikiUi).use(live).mount('#ui');
