import { hashHue, initials } from "../lib/format";
import type { Instance } from "../lib/types";

/** Emoji render at a different optical size than text, so they get a nudge. */
const EMOJI_RE = /\p{Extended_Pictographic}/u;

export default function InstanceIcon({
  instance,
  size = 40,
  radius,
}: {
  instance: Pick<Instance, "id" | "name" | "icon">;
  size?: number;
  radius?: number;
}) {
  const hue = hashHue(instance.id);
  const style: React.CSSProperties = {
    width: size,
    height: size,
    borderRadius: radius ?? Math.max(6, size * 0.24),
    display: "grid",
    placeItems: "center",
    flexShrink: 0,
    background: `linear-gradient(135deg, hsl(${hue} 52% 40%), hsl(${(hue + 45) % 360} 52% 27%))`,
    color: "#fff",
    fontWeight: 700,
    fontSize: size * 0.36,
    letterSpacing: "0.01em",
    overflow: "hidden",
    userSelect: "none",
  };

  const icon = instance.icon?.trim();

  if (icon && EMOJI_RE.test(icon)) {
    return (
      <span style={{ ...style, background: "var(--surface-3)", fontSize: size * 0.52 }}>
        {icon}
      </span>
    );
  }

  return <span style={style}>{initials(instance.name)}</span>;
}
