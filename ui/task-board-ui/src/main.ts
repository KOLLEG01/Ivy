import { createApp } from 'vue';
import '@ivy/ui/styles.css';
import TaskBoardUi from './TaskBoardUi.vue';
import { live } from './runtime';
createApp(TaskBoardUi).use(live).mount('#ui');
