import { useEffect, useState } from "react";
import { isTauri } from "./api";

export interface UpdateInfo {
  version: string;
  /** What's new (the release's description). */
  notes: string;
  /** Download and install; `progress` gets 0–1 (or null when the size is unknown). */
  install(progress: (fraction: number | null) => void): Promise<void>;
  restart(): Promise<void>;
}

const SIX_HOURS = 6 * 60 * 60 * 1000;

/** Checks GitHub Releases on launch and every six hours. Only checks:
 *  nothing is downloaded or installed until the user says so. */
export function useUpdater(): UpdateInfo | null {
  const [info, setInfo] = useState<UpdateInfo | null>(null);

  useEffect(() => {
    if (!isTauri) return;
    let canceled = false;

    const run = async () => {
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
      }
    };

    run();
    const timer = window.setInterval(run, SIX_HOURS);
    return () => {
      canceled = true;
      window.clearInterval(timer);
    };
  }, []);

  return info;
}
