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
