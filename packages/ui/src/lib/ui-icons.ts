import type { Component } from "vue";
import {
  AppWindow,
  BookOpen,
  Bot,
  Camera,
  ChartNoAxesCombined,
  ClipboardClock,
  Database,
  Funnel,
  LayoutDashboard,
  ListChecks,
  MessageCircle,
  Newspaper,
  NotebookText,
  Phone,
  Presentation,
  Settings,
  UserRound,
} from "@lucide/vue";

// One icon per registered UI icon key, shared by the app rail and the UI launcher.
const icons: Record<string, Component> = {
  ui: AppWindow,
  bot: Bot,
  "book-open": BookOpen,
  "notebook-text": NotebookText,
  "list-checks": ListChecks,
  "message-circle": MessageCircle,
  phone: Phone,
  camera: Camera,
  newspaper: Newspaper,
  settings: Settings,
  "layout-dashboard": LayoutDashboard,
  "chart-no-axes-combined": ChartNoAxesCombined,
  presentation: Presentation,
  database: Database,
  funnel: Funnel,
  "user-round": UserRound,
  "clipboard-clock": ClipboardClock,
};
export const uiIcon = (key: string): Component => icons[key] ?? AppWindow;
