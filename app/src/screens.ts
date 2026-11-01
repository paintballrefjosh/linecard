/** Terminals whose screen a script can read, by backend session id. */
const readers = new Map<string, () => string>();

export const screens = {
  register(session: string, read: () => string) {
    readers.set(session, read);
    return () => void (readers.get(session) === read && readers.delete(session));
  },
  read: (session: string): string | null => readers.get(session)?.() ?? null,
};
