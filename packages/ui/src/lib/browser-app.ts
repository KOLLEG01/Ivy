export interface BrowserAppControls {
  supported: boolean;
  installed: boolean;
  installAvailable: boolean;
  installBannerVisible: boolean;
  enabled: boolean;
  ready: boolean;
  busy: boolean;
  permission: NotificationPermission;
  error: string;
  message: string;
  install(): Promise<void>;
  dismissInstallBanner(): void;
  enable(): Promise<void>;
  disable(): Promise<void>;
  test(): Promise<void>;
  refresh(): Promise<void>;
}
