import {
  Cable,
  CornerDownRight,
  Lock,
  LockOpen,
  Minus,
  Monitor,
  Pencil,
  Plus,
  RotateCcw,
  ScreenShare,
  SquareTerminal,
  Undo2,
} from "lucide-react";
import type { ChangeKind, ConnectionKind } from "../types";

export const kindLabel: Record<ConnectionKind, string> = {
  ssh: "SSH",
  telnet: "Telnet",
  serial: "Serial",
  rdp: "RDP",
  vnc: "VNC",
};

/** Connection-type icon in its accent colour. SSH and Telnet share the
 *  terminal glyph; a lock / open-lock badge tells them apart. */
export function KindIcon({ kind, size = 16 }: { kind: ConnectionKind; size?: number }) {
  const style = { color: `var(--kind-${kind})` };
  const glyph = (() => {
    switch (kind) {
      case "ssh":
      case "telnet":
        return <SquareTerminal size={size} strokeWidth={1.75} />;
      case "serial":
        return <Cable size={size} strokeWidth={1.75} />;
      case "rdp":
        return <Monitor size={size} strokeWidth={1.75} />;
      case "vnc":
        return <ScreenShare size={size} strokeWidth={1.75} />;
    }
  })();
  const badge =
    kind === "ssh" ? (
      <Lock size={size * 0.55} strokeWidth={2.5} />
    ) : kind === "telnet" ? (
      <LockOpen size={size * 0.55} strokeWidth={2.5} />
    ) : null;
  return (
    <span className="kind-icon" style={style} aria-label={kindLabel[kind]} role="img">
      {glyph}
      {badge && <span className="kind-badge">{badge}</span>}
    </span>
  );
}

export const changeLabel: Record<ChangeKind, string> = {
  added: "Added",
  edited: "Edited",
  moved: "Moved",
  deleted: "Deleted",
  restored: "Restored",
  reverted: "Reverted",
};

/** Small glyph so changes read without relying on colour alone. */
export function ChangeGlyph({ kind, size = 12 }: { kind: ChangeKind; size?: number }) {
  const props = { size, strokeWidth: 2.5 };
  switch (kind) {
    case "added":
    case "restored":
      return <Plus {...props} />;
    case "edited":
      return <Pencil {...props} />;
    case "moved":
      return <CornerDownRight {...props} />;
    case "deleted":
      return <Minus {...props} />;
    case "reverted":
      return <RotateCcw {...props} />;
  }
}

export { Undo2 };
