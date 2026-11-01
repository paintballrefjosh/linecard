import { useMemo } from "react";
import qrcode from "qrcode-generator";

/** `text` as a QR code. Always black on white with a quiet zone, whatever the
 *  theme: scanners need the contrast. */
export function QrCode({ text, label, size = 200 }: { text: string; label: string; size?: number }) {
  const { path, n } = useMemo(() => {
    const qr = qrcode(0, "M");
    qr.addData(text);
    qr.make();
    const n = qr.getModuleCount();
    let d = "";
    for (let r = 0; r < n; r++) for (let c = 0; c < n; c++) if (qr.isDark(r, c)) d += `M${c} ${r}h1v1h-1z`;
    return { path: d, n };
  }, [text]);
  const quiet = 4;
  return (
    <svg
      role="img"
      aria-label={label}
      width={size}
      height={size}
      viewBox={`${-quiet} ${-quiet} ${n + quiet * 2} ${n + quiet * 2}`}
      shapeRendering="crispEdges"
      className="qr-code"
    >
      <rect x={-quiet} y={-quiet} width={n + quiet * 2} height={n + quiet * 2} fill="#fff" />
      <path d={path} fill="#000" />
    </svg>
  );
}
