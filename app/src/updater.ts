import { useEffect, useState } from "react";
import { getBackend, isTauri } from "./api";

export interface UpdateInfo {
  version: string;
  /** What's new (the release's description). */
  notes: string;
  /** Download and install; `progress` gets 0–1 (or null when the size is unknown). */
  install(progress: (fraction: number | null) => void): Promise<void>;
  restart(): Promise<void>;
}

const HOUR = 60 * 60 * 1000;
/** Coming back to the window checks again if the last check was this long ago. */
const STALE = 15 * 60 * 1000;

/** Checks GitHub Releases on launch, hourly, and when you come back to the window after a while. Only checks:
 *  nothing is downloaded or installed until the user says so. */
export function useUpdater(): UpdateInfo | null {
  const [info, setInfo] = useState<UpdateInfo | null>(null);

  useEffect(() => {
    if (!isTauri) return;
    let canceled = false;

    let last = 0;
    const run = async () => {
      last = Date.now();
      try {
        const { check } = await import("@tauri-apps/plugin-updater");
        const { relaunch } = await import("@tauri-apps/plugin-process");
        const update = await check();
        if (!update || canceled) return;
        setInfo({
          version: update.version,
          notes: update.body ?? "",
          async install(progress) {
            let total: number | null = null;
            let done = 0;
            await update.downloadAndInstall((e) => {
              if (e.event === "Started") total = e.data.contentLength ?? null;
              else if (e.event === "Progress") {
                done += e.data.chunkLength;
                progress(total ? Math.min(1, done / total) : null);
              } else progress(1);
            });
          },
          restart: () => relaunch(),
        });
      } catch (err) {
        console.warn("update check failed", err);
        void getBackend().then((b) => b.logDebug("update", `update check failed: ${String(err)}`));
      }
    };

    // Timers pause while the computer sleeps or the window is hidden, so a
    // long-running session also checks when it is looked at again.
    const recheck = () => {
      if (document.visibilityState === "visible" && Date.now() - last > STALE) void run();
    };
    run();
    const timer = window.setInterval(run, HOUR);
    window.addEventListener("focus", recheck);
    document.addEventListener("visibilitychange", recheck);
    return () => {
      canceled = true;
      window.clearInterval(timer);
      window.removeEventListener("focus", recheck);
      document.removeEventListener("visibilitychange", recheck);
    };
  }, []);

  return info;
}
