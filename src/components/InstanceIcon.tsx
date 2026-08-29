import { useState } from "react";

import { hashHue, initials } from "../lib/format";
import type { Instance } from "../lib/types";

/** Emoji render at a different optical size than text, so they get a nudge. */
const EMOJI_RE = /\p{Extended_Pictographic}/u;

export default function InstanceIcon({
  instance,
  size = 40,
  radius,
}: {
  instance: Pick<Instance, "id" | "name" | "icon"> & {
    pack?: Instance["pack"];
  };
  size?: number;
  radius?: number;
}) {
  // Remote art can fail — offline, or the project pulled the image.
  const [artFailed, setArtFailed] = useState(false);

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

  // An explicit icon wins — pack artwork is a default, not a lock.
  if (icon && EMOJI_RE.test(icon)) {
    return (
      <span style={{ ...style, background: "var(--surface-3)", fontSize: size * 0.52 }}>
        {icon}
      </span>
    );
  }

  if (!icon && instance.pack?.iconUrl && !artFailed) {
    return (
      <img
        src={instance.pack.iconUrl}
        alt=""
        loading="lazy"
        style={{ ...style, background: "var(--surface-3)", objectFit: "cover" }}
        onError={() => setArtFailed(true)}
      />
    );
  }

  return <span style={style}>{initials(instance.name)}</span>;
}
