import { createApp } from "vue";
import "@ivy/ui/styles.css";
import DataCollector from "./DataCollector.vue";
import { live } from "./runtime";
createApp(DataCollector).use(live).mount("#ui");
