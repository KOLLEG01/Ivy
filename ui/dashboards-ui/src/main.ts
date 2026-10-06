import { createApp } from "vue";
import "@ivy/ui/styles.css";
import App from "./App.vue";
import { live } from "./runtime";
createApp(App).use(live).mount("#ui");
