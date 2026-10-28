import { useEffect, useState } from "react";

export type ThemePref = "system" | "light" | "dark";
export type Theme = "light" | "dark";

const KEY = "linecard.theme";
const query = () => window.matchMedia("(prefers-color-scheme: dark)");

function readPref(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

/** Theme preference (defaults to following the OS) and the resolved theme.
 *  Follows OS changes live while the preference is "system". */
export function useTheme() {
  const [pref, setPrefState] = useState<ThemePref>(readPref);
  const [systemDark, setSystemDark] = useState(() => query().matches);

  useEffect(() => {
    const mq = query();
    const onChange = (e: MediaQueryListEvent) => setSystemDark(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  const theme: Theme = pref === "system" ? (systemDark ? "dark" : "light") : pref;

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  const setPref = (p: ThemePref) => {
    setPrefState(p);
    try {
      localStorage.setItem(KEY, p);
    } catch {
      /* storage unavailable: keep in memory only */
    }
  };

  return { pref, setPref, theme };
}

/** xterm.js color schemes matched to the app theme. */
export const terminalThemes = {
  dark: {
    background: "#161b22",
    foreground: "#d6dde6",
    cursor: "#7fb0ff",
    selectionBackground: "#2d4a7a",
    black: "#1c222b",
    red: "#f0716b",
    green: "#7fcf8a",
    yellow: "#e8c06a",
    blue: "#7fb0ff",
    magenta: "#c79bf2",
    cyan: "#6fd0d8",
    white: "#d6dde6",
    brightBlack: "#5b6675",
    brightRed: "#ff8f88",
    brightGreen: "#9be3a5",
    brightYellow: "#f5d58c",
    brightBlue: "#a3c7ff",
    brightMagenta: "#dcb8ff",
    brightCyan: "#93e3ea",
    brightWhite: "#f4f7fa",
  },
  light: {
    background: "#fbfcfd",
    foreground: "#1f2733",
    cursor: "#2f6fd6",
    selectionBackground: "#cfe0fb",
    black: "#1f2733",
    red: "#c33b35",
    green: "#2c8a3e",
    yellow: "#9a6b00",
    blue: "#2f6fd6",
    magenta: "#8a4fc2",
    cyan: "#1a8792",
    white: "#c9d1db",
    brightBlack: "#6b7684",
    brightRed: "#e0544d",
    brightGreen: "#3aa851",
    brightYellow: "#b88400",
    brightBlue: "#4a86ea",
    brightMagenta: "#a46bdb",
    brightCyan: "#25a3b0",
    brightWhite: "#ffffff",
  },
} as const;
