import { create } from "zustand";

const NOTIFICATION_DURATION_MS = 5000;

export interface AppNotificationAnchor {
  x: number;
  y: number;
  placement: "above" | "below";
}

export interface AppNotification {
  title: string;
  description: string;
  /** Viewport point for a notification attached to a rendered game object. */
  anchor?: AppNotificationAnchor;
}

interface AppNotificationState {
  notification: AppNotification | null;
  expiresAt: number;
  showNotification: (notification: AppNotification) => void;
  clearNotification: () => void;
}

export const useAppNotificationStore = create<AppNotificationState>((set) => ({
  notification: null,
  expiresAt: 0,
  showNotification: (notification) =>
    set({ notification, expiresAt: Date.now() + NOTIFICATION_DURATION_MS }),
  clearNotification: () => set({ notification: null, expiresAt: 0 }),
}));
