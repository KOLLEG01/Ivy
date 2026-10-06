import { createApp } from "vue";
import "@ivy/ui/styles.css";
import SecretaryUi from "./SecretaryUi.vue";
import { live } from "./runtime";
createApp(SecretaryUi).use(live).mount("#ui");
