import { onBeforeUnmount, onMounted, reactive } from "vue";
import type { BrowserAppControls } from "../../ui/src/lib/browser-app";

interface InstallPrompt extends Event {
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}
interface PushStatus {
  publicKey: string;
  subscribed: boolean;
  lastError: string | null;
}

export function useBrowserApp(base: URL): BrowserAppControls {
  let registration: ServiceWorkerRegistration | null = null;
  let prompt: InstallPrompt | null = null;
  let publicKey = "";
  const standalone = matchMedia("(display-mode: standalone)");
  const isInstalled = () =>
    standalone.matches ||
    (navigator as Navigator & { standalone?: boolean }).standalone === true;
  // Shared by every UI/tab of this Hive, but local to this browser profile.
  const installChoiceKey = "ivy.install-banner:" + base.pathname;
  let installHandled = false;
  try {
    installHandled = localStorage.getItem(installChoiceKey) === "hidden";
  } catch {
    /* Storage is optional; dismissal still lasts for the current page. */
  }
  const supported =
    window.isSecureContext &&
    "serviceWorker" in navigator &&
    "PushManager" in window &&
    "Notification" in window;
  const app = reactive<BrowserAppControls>({
    supported,
    installed: isInstalled(),
    installAvailable: false,
    installBannerVisible:
      window.isSecureContext &&
      "serviceWorker" in navigator &&
      !isInstalled() &&
      !installHandled,
    enabled: false,
    ready: false,
    busy: false,
    permission: supported ? Notification.permission : "default",
    error: "",
    message: "",
    install: () =>
      run(async () => {
        const installation = prompt;
        if (!installation) return;
        await installation.prompt();
        await installation.userChoice;
        prompt = null;
        app.installAvailable = false;
        // Both accepting and explicitly dismissing the native prompt settle the banner.
        dismissInstallBanner();
      }),
    dismissInstallBanner,
    enable: () =>
      run(async () => {
        if (!registration)
          throw new Error("The browser app is still starting. Try again.");
        // Must remain directly connected to the button gesture, especially on iOS.
        app.permission = await Notification.requestPermission();
        if (app.permission !== "granted") return;
        const bytes = Uint8Array.from(
          atob(publicKey.replace(/-/g, "+").replace(/_/g, "/")),
          (c) => c.charCodeAt(0),
        );
        let subscription = await registration.pushManager.getSubscription();
        if (
          subscription?.options.applicationServerKey &&
          !sameKey(subscription.options.applicationServerKey, bytes)
        ) {
          await subscription.unsubscribe();
          subscription = null;
        }
        subscription ??= await registration.pushManager.subscribe({
          userVisibleOnly: true,
          applicationServerKey: bytes,
        });
        await request({
          action: "subscribe",
          subscription: subscription.toJSON(),
        });
        app.enabled = true;
      }),
    disable: () =>
      run(async () => {
        const subscription = await registration?.pushManager.getSubscription();
        if (subscription) {
          await request({
            action: "unsubscribe",
            endpoint: subscription.endpoint,
          });
          app.enabled = false;
          await subscription.unsubscribe();
        }
        app.enabled = false;
      }),
    test: () =>
      run(async () => {
        const subscription = await registration?.pushManager.getSubscription();
        if (!subscription) throw new Error("Enable notifications first.");
        await request({ action: "test", endpoint: subscription.endpoint });
        app.message =
          "Test notification queued. Delivery depends on your browser and device settings.";
      }),
    refresh: () =>
      run(async () => {
        if (!supported) return;
        app.permission = Notification.permission;
        registration ??= await register();
        const subscription = await registration.pushManager.getSubscription();
        const status = (await request({
          action: "status",
          endpoint: subscription?.endpoint,
        })) as PushStatus;
        publicKey = status.publicKey;
        app.enabled =
          app.permission === "granted" && !!subscription && status.subscribed;
        app.error = status.lastError ?? "";
        app.ready = true;
      }),
  });
  function dismissInstallBanner(): void {
    app.installBannerVisible = false;
    try {
      localStorage.setItem(installChoiceKey, "hidden");
    } catch {
      /* Browsers may disable persistent storage. */
    }
  }
  async function request(value: unknown): Promise<unknown> {
    const response = await fetch(new URL("api/browser-push", base), {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(value),
      signal: AbortSignal.timeout(15_000),
    });
    const result = await response.json();
    if (!response.ok)
      throw new Error(
        result?.error?.message ??
          "Unable to update notifications. Sign in and try again.",
      );
    return result;
  }
  async function run(work: () => Promise<void>): Promise<void> {
    if (app.busy) return;
    app.busy = true;
    app.error = "";
    app.message = "";
    try {
      await work();
    } catch (error) {
      app.error =
        error instanceof Error
          ? error.message
          : "Unable to update browser app settings.";
    } finally {
      app.busy = false;
    }
  }
  async function register(): Promise<ServiceWorkerRegistration> {
    const value = await navigator.serviceWorker.register(
      new URL("app-worker.js", base),
      { scope: base.pathname, updateViaCache: "none" },
    );
    if (value.active) return value;
    const worker = value.installing ?? value.waiting;
    if (!worker) throw new Error("The browser app could not start. Try again.");
    await new Promise<void>((resolve, reject) => {
      const finish = (error?: Error) => {
        clearTimeout(timer);
        worker.removeEventListener("statechange", changed);
        error ? reject(error) : resolve();
      };
      const changed = () => {
        if (worker.state === "activated") finish();
        else if (worker.state === "redundant")
          finish(new Error("The browser app could not start."));
      };
      const timer = setTimeout(
        () =>
          finish(
            new Error("The browser app took too long to start. Try again."),
          ),
        15_000,
      );
      worker.addEventListener("statechange", changed);
      changed();
    });
    return value;
  }
  const capturePrompt = (event: Event) => {
    event.preventDefault();
    prompt = event as InstallPrompt;
    app.installAvailable = true;
  };
  const installed = () => {
    app.installed = true;
    app.installAvailable = false;
    prompt = null;
    dismissInstallBanner();
  };
  const displayChanged = () => {
    app.installed = isInstalled();
    if (app.installed) dismissInstallBanner();
  };
  const choiceChanged = (event: StorageEvent) => {
    if (event.key === installChoiceKey && event.newValue === "hidden")
      app.installBannerVisible = false;
  };
  const links = [
    ["manifest", "app/manifest.webmanifest"],
    ["apple-touch-icon", "app/icon-180.png"],
  ];
  for (const [rel, path] of links) {
    if (document.querySelector(`link[rel="${rel}"]`)) continue;
    const link = document.createElement("link");
    link.rel = rel!;
    link.href = new URL(path!, base).href;
    document.head.append(link);
  }
  if (!document.querySelector('meta[name="theme-color"]')) {
    const theme = document.createElement("meta");
    theme.name = "theme-color";
    theme.content = "#ffffff";
    document.head.append(theme);
  }
  window.addEventListener("beforeinstallprompt", capturePrompt);
  window.addEventListener("appinstalled", installed);
  window.addEventListener("storage", choiceChanged);
  standalone.addEventListener("change", displayChanged);
  onMounted(() => {
    if (app.installed) dismissInstallBanner();
    if (supported) void app.refresh();
    else if (window.isSecureContext && "serviceWorker" in navigator)
      void register().catch(() => undefined);
  });
  onBeforeUnmount(() => {
    window.removeEventListener("beforeinstallprompt", capturePrompt);
    window.removeEventListener("appinstalled", installed);
    window.removeEventListener("storage", choiceChanged);
    standalone.removeEventListener("change", displayChanged);
  });
  return app;
}
function sameKey(buffer: ArrayBuffer, expected: Uint8Array): boolean {
  const actual = new Uint8Array(buffer);
  return (
    actual.length === expected.length &&
    actual.every((value, index) => value === expected[index])
  );
}
