/** A script to run now and then while Linecard is open. */
export interface Schedule {
  id: string;
  /** The script's path. */
  script: string;
  /** Every this many minutes (counted from when Linecard started or the last run)... */
  every?: number;
  /** ...or once a day at this local time, "HH:MM". */
  at?: string;
}

/** Today's `HH:MM` as a time, or null if it is not a time. */
export function timeToday(at: string, now: Date): Date | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(at.trim());
  if (!m) return null;
  const [h, min] = [Number(m[1]), Number(m[2])];
  if (h > 23 || min > 59) return null;
  const t = new Date(now);
  t.setHours(h, min, 0, 0);
  return t;
}

/** Whether `rule` should start now.
 *
 *  `last` is when it last started (null: not since Linecard started) and
 *  `startedAt` when Linecard started. A daily run that was missed because
 *  Linecard wasn't running at that time is not made up; one missed because
 *  the computer was asleep is, once it wakes. */
export function isDue(rule: Schedule, now: Date, last: number | null, startedAt: number): boolean {
  if (rule.every !== undefined) {
    if (!(rule.every >= 1)) return false;
    return now.getTime() - (last ?? startedAt) >= rule.every * 60_000;
  }
  if (rule.at !== undefined) {
    const t = timeToday(rule.at, now);
    if (!t) return false;
    return now >= t && (last ?? 0) < t.getTime() && startedAt <= t.getTime();
  }
  return false;
}

/** How a schedule reads: "every 30 minutes", "daily at 06:00". */
export function describe(rule: Schedule): string {
  if (rule.every !== undefined) return rule.every === 1 ? "every minute" : `every ${rule.every} minutes`;
  return `daily at ${rule.at ?? "?"}`;
}
