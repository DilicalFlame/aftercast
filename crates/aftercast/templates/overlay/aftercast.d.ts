// Types for the aftercast overlay SDK (plugin API v1): `window.aftercast`, or
// `import ac from "/sdk/aftercast.js"`.

export interface Store<T> {
  get(): T;
  /** Calls `fn` at once with the current value; returns the unsubscribe function. */
  subscribe(fn: (value: T) => void): () => void;
}

export interface Message<D = any> {
  overlay: string;
  action: "show" | "hide" | "update";
  data: D;
}

export interface Theme {
  bg: string;
  accent: string;
  fg: string;
  muted: string;
  fontMono: string;
  fontSans: string;
}

export interface Aftercast {
  id: string;
  api: number;
  name: string;
  version: string;
  mode: "live" | "demo" | "thumb";
  features: string[];
  /** The values of overlay.toml's [[param]]s, defaults filled in. */
  params: Store<Record<string, any>>;
  /** The skin in effect (one of overlay.toml's [[skin]] ids). */
  skin: Store<string>;
  theme: Store<Theme>;
  frame: Store<{ width: number; height: number; scale: number }>;
  /** `[overlay.sound] volume`, 0 to 1. */
  volume: Store<number>;
  /** Names of overlay.toml's [[scenario]]s. */
  scenarios: string[];
  /** The last show/hide message of a stream (brb, now, question, toast, ...), or null. */
  state<D = any>(stream: string): Store<Message<D> | null>;
  /** Every message of a stream; the current show/hide state is replayed first. */
  on<D = any>(stream: string, fn: (message: Message<D>) => void): () => void;
  ready(fn: () => void): void;
  url(path: string): string;
  now(): number;
  applyScenario(name?: string): boolean;
}

declare global {
  interface Window {
    aftercast: Aftercast;
  }
}

declare const ac: Aftercast;
export default ac;
