import { createContext, useContext } from "react";

/** Per-device preferences (never synced). Stored by the backend; defaults
 *  live here so new options need no migration. */
export interface Preferences {
  /** Highlighting text in a terminal copies it (SecureCRT / PuTTY style). */
  copyOnSelect: boolean;
  /** What right-click does inside a terminal. */
  rightClick: "paste" | "menu";
  /** Show multi-line text before pasting it into a session. */
  confirmMultilinePaste: boolean;
}

export const defaultPrefs: Preferences = {
  copyOnSelect: true,
  rightClick: "paste",
  confirmMultilinePaste: true,
};

export function withDefaults(stored: Partial<Preferences> | null | undefined): Preferences {
  return { ...defaultPrefs, ...(stored ?? {}) };
}

export const PrefsContext = createContext<{
  prefs: Preferences;
  update(patch: Partial<Preferences>): void;
}>({ prefs: defaultPrefs, update: () => {} });

export const usePrefs = () => useContext(PrefsContext);

/** Lines in pasted text, ignoring one trailing newline ("show run\n" is one line). */
export function pasteLineCount(text: string): number {
  return text.replace(/\r\n?/g, "\n").replace(/\n$/, "").split("\n").length;
}
