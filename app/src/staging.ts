import { getBackend } from "./api";

/** What was dropped: files (with their path below the drop) and folders. */
export interface Dropped {
  /** Path inside the drop, "/"-separated; the first part is the dropped name. */
  rel: string;
  file: File | null; // null for a folder
}

const PIECE = 4 * 1024 * 1024;

/** True when a drag carries files from outside the app (not a tab or text). */
export const hasFiles = (e: { dataTransfer: DataTransfer | null }) => !!e.dataTransfer?.types.includes("Files");

const readAll = (reader: FileSystemDirectoryReader) =>
  new Promise<FileSystemEntry[]>((resolve, reject) => {
    const all: FileSystemEntry[] = [];
    const more = () =>
      reader.readEntries((batch) => {
        if (batch.length === 0) resolve(all);
        else {
          all.push(...batch);
          more();
        }
      }, reject);
    more();
  });

const fileOf = (entry: FileSystemFileEntry) => new Promise<File>((resolve, reject) => entry.file(resolve, reject));

async function walk(entry: FileSystemEntry, rel: string, out: Dropped[]): Promise<void> {
  if (entry.isFile) {
    out.push({ rel, file: await fileOf(entry as FileSystemFileEntry) });
  } else if (entry.isDirectory) {
    out.push({ rel, file: null });
    for (const child of await readAll((entry as FileSystemDirectoryEntry).createReader()))
      await walk(child, `${rel}/${child.name}`, out);
  }
}

/** Everything in a drop, folders opened. Call this straight from the drop
 *  handler: the browser forgets the items once it returns. */
export function collectDrop(dt: DataTransfer): Promise<Dropped[]> {
  // Entries open folders; where a browser gives none, fall back to the plain file.
  const items = [...dt.items]
    .filter((item) => item.kind === "file")
    .map((item) => ({ entry: item.webkitGetAsEntry?.() ?? null, file: item.getAsFile() }));
  return (async () => {
    const out: Dropped[] = [];
    for (const { entry, file } of items) {
      if (entry) await walk(entry, entry.name, out);
      else if (file) out.push({ rel: file.name, file });
    }
    return out;
  })();
}

/** Write a drop into a temporary folder on this computer so it can be
 *  uploaded like any local file (the window gives no paths for dropped
 *  files). `paths` are the dropped items' places in that folder; `end`
 *  removes it. */
export async function stageDrop(
  dropped: Dropped[],
  progress: (bytes: number) => void,
): Promise<{ paths: string[]; end(): Promise<void> }> {
  const b = await getBackend();
  const dir = await b.stageBegin();
  const sep = dir.includes("\\") ? "\\" : "/";
  try {
    let done = 0;
    for (const d of dropped) {
      if (!d.file) {
        await b.stageMkdir(dir, d.rel);
        continue;
      }
      if (d.file.size === 0) await b.stageWrite(dir, d.rel, 0, new Uint8Array());
      for (let at = 0; at < d.file.size; at += PIECE) {
        const piece = new Uint8Array(await d.file.slice(at, at + PIECE).arrayBuffer());
        await b.stageWrite(dir, d.rel, at, piece);
        done += piece.length;
        progress(done);
      }
    }
  } catch (e) {
    await b.stageEnd(dir).catch(() => {});
    throw e;
  }
  const tops = [...new Set(dropped.map((d) => d.rel.split("/")[0]))];
  return { paths: tops.map((t) => `${dir}${sep}${t}`), end: () => b.stageEnd(dir) };
}
