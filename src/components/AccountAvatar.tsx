import { UserRound } from "lucide-react";

import { hashHue, initials } from "../lib/format";

/**
 * The little square that stands for an account.
 *
 * `InstanceIcon` is the same idea for an instance or a server, and this is a
 * component for the same reason: the gradient was pasted verbatim into the
 * accounts modal, the Settings accounts list and the sidebar footer, over
 * three `.avatar` blocks that had drifted to 32/30/26px boxes and 12/11/11px
 * initials — the same box twice at one font size, and one font size twice at
 * different boxes. Sizes derive from the box here, so there is nothing left to
 * disagree about.
 *
 * Avatars are generated locally from the profile id. Fetching skin heads would
 * mean sending every account UUID to a third party.
 */
export default function AccountAvatar({
  account,
  size = 32,
}: {
  /**
   * Structural rather than an `AccountView`, so a caller need not have one —
   * and nullable, because the sidebar draws this slot before anybody has
   * signed in, and then it is a person icon rather than initials.
   */
  account: { id: string; username: string } | null;
  size?: number;
}) {
  const style: React.CSSProperties = {
    width: size,
    height: size,
    borderRadius: Math.max(6, size * 0.25),
    display: "grid",
    placeItems: "center",
    flexShrink: 0,
    overflow: "hidden",
    color: "#fff",
    fontWeight: 700,
    fontSize: size * 0.38,
    letterSpacing: "0.02em",
    userSelect: "none",
  };

  if (!account) {
    return (
      <span
        style={{
          ...style,
          background: "var(--surface-3)",
          color: "var(--text-faint)",
        }}
      >
        <UserRound size={size * 0.58} />
      </span>
    );
  }

  const hue = hashHue(account.id);
  return (
    <span
      style={{
        ...style,
        background: `linear-gradient(135deg, hsl(${hue} 55% 42%), hsl(${(hue + 40) % 360} 55% 30%))`,
      }}
    >
      {initials(account.username)}
    </span>
  );
}
