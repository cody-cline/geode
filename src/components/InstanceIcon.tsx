import { useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";

import { hashHue, initials } from "../lib/format";
import type { PackSource } from "../lib/types";

/**
 * The little square that stands for an instance or a server.
 *
 * In order: an image the user uploaded, then a modpack's own artwork, then
 * initials on a colour generated from the id. The id is the only thing needed
 * for that last one, which is why a server — which has no artwork of its own
 * unless someone gives it one — can use this too.
 */
export default function InstanceIcon({
  instance,
  size = 40,
  radius,
}: {
  /**
   * Structural rather than an `Instance`, so a server can be passed as
   * `{ id, name, iconPath }` without pretending to be one.
   */
  instance: {
    id: string;
    name: string;
    iconPath?: string | null;
    pack?: PackSource | null;
  };
  size?: number;
  radius?: number;
}) {
  // Art can fail: a project can pull its image, and an uploaded file can be
  // deleted out from under us.
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

  const uploaded = instance.iconPath;

  // An uploaded image wins — pack artwork is a default, not a lock.
  if (uploaded && !artFailed) {
    return (
      <img
        src={convertFileSrc(uploaded)}
        alt=""
        style={{ ...style, background: "var(--surface-3)", objectFit: "cover" }}
        onError={() => setArtFailed(true)}
      />
    );
  }

  if (!uploaded && instance.pack?.iconUrl && !artFailed) {
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
