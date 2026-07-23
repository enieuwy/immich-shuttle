/**
 * Display names for a set of users: first name only, disambiguated with the
 * last-name initial(s) when two or more share a first name.
 */
export function userDisplayNames(users: { name: string }[]): string[] {
  const firsts = users.map((u) => {
    const trimmed = u.name.trim();
    return trimmed.split(/\s+/)[0] || trimmed;
  });
  const counts = new Map<string, number>();
  for (const f of firsts) {
    const key = f.toLowerCase();
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return users.map((u, i) => {
    const f = firsts[i];
    if ((counts.get(f.toLowerCase()) ?? 0) <= 1) {
      return f;
    }
    const initials = u.name
      .trim()
      .split(/\s+/)
      .slice(1)
      .map((part) => part.charAt(0).toUpperCase())
      .filter(Boolean)
      .join("");
    return initials ? `${f} ${initials}` : u.name.trim();
  });
}

export interface AvatarStyle {
  bg: string;
  fg: string;
}

/**
 * Badge colors: a cohesive, muted family (uniform chroma, two luminance tiers)
 * tuned to sit calmly on the dark, desaturated surfaces rather than the punchy
 * saturated primaries that read as garish stickers on near-black. Hues still
 * follow Immich's avatar-color names so identity stays per-person; every fg/bg
 * pair clears WCAG 4.5:1 for legibility at 8-9px (enforced by a test). The
 * warm light tier (orange/amber/yellow) uses dark text.
 */
const AVATAR_COLORS: Record<string, AvatarStyle> = {
  primary: { bg: "#5366a7", fg: "#ffffff" },
  pink: { bg: "#90556f", fg: "#ffffff" },
  red: { bg: "#95564e", fg: "#ffffff" },
  yellow: { bg: "#cabf7f", fg: "#332800" },
  blue: { bg: "#3d6f98", fg: "#ffffff" },
  green: { bg: "#3a7957", fg: "#ffffff" },
  purple: { bg: "#765d92", fg: "#ffffff" },
  orange: { bg: "#e9b089", fg: "#471900" },
  gray: { bg: "#656970", fg: "#ffffff" },
  amber: { bg: "#dcb77f", fg: "#3f2100" },
};

/** Hash fallback rotation for servers that omit `avatarColor`. */
const FALLBACK_COLORS = ["blue", "green", "purple", "orange", "pink", "red", "amber", "gray"];

/**
 * Deterministic per-person badge color: the user's Immich avatar color when
 * known, else a stable hash of the user id over the same palette.
 */
export function avatarStyle(user: { id: string; avatar_color?: string | null }): AvatarStyle {
  const named = user.avatar_color ? AVATAR_COLORS[user.avatar_color] : undefined;
  if (named) {
    return named;
  }
  // FNV-1a over the id: stable across sessions, spreads well for UUIDs.
  let hash = 0x811c9dc5;
  for (let i = 0; i < user.id.length; i += 1) {
    hash ^= user.id.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  const key = FALLBACK_COLORS[(hash >>> 0) % FALLBACK_COLORS.length];
  return AVATAR_COLORS[key];
}
