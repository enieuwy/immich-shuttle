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
 * Badge colors matching Immich's avatar palette, so a person renders with the
 * same identity color here as in the Immich web app. Yellow and amber get dark
 * text because white monograms are unreadable on them at badge size.
 */
const AVATAR_COLORS: Record<string, AvatarStyle> = {
  primary: { bg: "#4250af", fg: "#ffffff" },
  pink: { bg: "#f472b6", fg: "#ffffff" },
  red: { bg: "#ef4444", fg: "#ffffff" },
  yellow: { bg: "#eab308", fg: "#292103" },
  blue: { bg: "#3b82f6", fg: "#ffffff" },
  green: { bg: "#16a34a", fg: "#ffffff" },
  purple: { bg: "#9333ea", fg: "#ffffff" },
  orange: { bg: "#ea580c", fg: "#ffffff" },
  gray: { bg: "#4b5563", fg: "#ffffff" },
  amber: { bg: "#d97706", fg: "#2b1a02" },
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
