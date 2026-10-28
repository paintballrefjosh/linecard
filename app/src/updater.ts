import { useEffect, useState } from "react";
import { isTauri } from "./api";

export interface UpdateInfo {
  version: string;
  notes: string;
  /** Downloads (if needed), installs and restarts the app. */
  installAndRestart(): Promise<void>;
}

const SIX_HOURS = 6 * 60 * 60 * 1000;

/** Checks GitHub Releases on launch and every six hours, downloading in the
 *  background so "Restart now" is instant. Never restarts on its own. */
export function useUpdater(): UpdateInfo | null {
  const [info, setInfo] = useState<UpdateInfo | null>(null);

  useEffect(() => {
    if (!isTauri) return;
    let cancelled = false;

    const run = async () => {
      try {
        const { check } = await import("@tauri-apps/plugin-updater");
        const { relaunch } = await import("@tauri-apps/plugin-process");
        const update = await check();
        if (!update || cancelled) return;
        await update.download();
        if (cancelled) return;
        setInfo({
          version: update.version,
          notes: update.body ?? "",
          async installAndRestart() {
            await update.install();
            await relaunch();
          },
        });
      } catch (err) {
        console.warn("update check failed", err);
      }
    };

    run();
    const timer = window.setInterval(run, SIX_HOURS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  return info;
}
