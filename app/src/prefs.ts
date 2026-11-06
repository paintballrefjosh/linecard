import type { Schedule } from "./schedule";
import { createContext, useContext } from "react";
import type { SerializedDockview } from "dockview-react";

/** Open tabs and pane layout saved under a name. */
export interface SavedWorkspace {
  name: string;
  savedAt: number;
  layout: SerializedDockview;
}

/** Per-device preferences (never synced). Stored by the backend; defaults
 *  live here so new options need no migration. */
export interface Preferences {
  /** Highlighting text in a terminal copies it (SecureCRT / PuTTY style). */
  copyOnSelect: boolean;
  /** What right-click does inside a terminal. */
  rightClick: "paste" | "menu";
  /** Show multi-line text before pasting it into a session. */
  confirmMultilinePaste: boolean;
  /** Color keywords (down, err-disabled, up…) in terminal output. */
  highlightKeywords: boolean;
  workspaces: SavedWorkspace[];
  /** Folders collapsed in the connection tree (the rest are open). */
  closedFolders: string[];
  /** Ask before closing a tab whose session is open. */
  confirmCloseTab: boolean;
  /** Show the saved-command button bar above the tabs. */
  showButtonBar: boolean;
  /** Scripts to start when a connection logs in (per computer: paths differ). */
  onConnect: { connectionId: string; script: string }[];
  /** Scripts to run now and then while Linecard is open (per computer). */
  schedules: Schedule[];
  /** Python for scripts (empty: find one on this computer). */
  pythonPath: string;
  /** Ask once before quitting with sessions open. */
  confirmQuit: boolean;
  /** Connections opened lately, newest first (ids; "quick-…" for quick connects). */
  recent: string[];
  /** Terminal text size in points (Ctrl+scroll wheel or pinch changes it). */
  fontSize: number;
  /** Tabs show the title the session sets (as SecureCRT does) instead of the connection's name. */
  tabTitleFromSession: boolean;
}

export const defaultPrefs: Preferences = {
  copyOnSelect: true,
  rightClick: "paste",
  confirmMultilinePaste: true,
  highlightKeywords: true,
  workspaces: [],
  closedFolders: [],
  confirmCloseTab: true,
  confirmQuit: true,
  showButtonBar: false,
  pythonPath: "",
  onConnect: [],
  schedules: [],
  recent: [],
  fontSize: 13,
  tabTitleFromSession: true,
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
