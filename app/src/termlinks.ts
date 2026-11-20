// Links and clipboard requests from programs running in a terminal.

/** The most a program may put on the clipboard at once (OSC 52). */
export const OSC52_MAX = 1 << 20;

/**
 * The text an OSC 52 sequence asks to copy, or null to ignore it.
 *
 * The payload is `<targets>;<base64>`, e.g. `c;aGk=`. A `?` asks to read the
 * clipboard back, which is never allowed: a remote host could read whatever
 * you copied last. An empty payload clears the clipboard in some terminals;
 * here it is ignored.
 */
export function osc52Text(payload: string): string | null {
  const at = payload.indexOf(";");
  if (at < 0) return null;
  const data = payload.slice(at + 1).replace(/\s+/g, "");
  if (!data || data === "?" || data.length > Math.ceil(OSC52_MAX / 3) * 4) return null;
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(data)) return null;
  try {
    const bin = atob(data);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

/** Only web and mail links open from a terminal: never files, scripts or app URLs. */
export function openableUrl(uri: string): boolean {
  try {
    return ["http:", "https:", "mailto:"].includes(new URL(uri).protocol);
  } catch {
    return false;
  }
}
