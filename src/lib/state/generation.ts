/**
 * Latest-wins guard for async store commits.
 *
 * Every store in this directory reads through async IPC and then writes a shared
 * `writable`. Without a guard those writes land in completion order rather than
 * issue order, so a slow earlier response commits after a newer one and silently
 * reverts the store: a completed job back to running, a deleted profile
 * resurrected, a cleared history repopulated, an ejected card back in the picker.
 * `albumsState.loadAlbums` already solved this with a local counter; this is that
 * pattern extracted so every store spells it the same way.
 *
 * ```ts
 * const loads = createGeneration();
 *
 * async function load() {
 *   const isCurrent = loads.begin();
 *   const rows = await fetchRows();
 *   if (!isCurrent()) return;
 *   state.set(rows);
 * }
 * ```
 *
 * Re-check `isCurrent()` after *every* await in a multi-step read, not just the
 * first — each one is a point where a newer request can overtake this one.
 */
export type Generation = {
  /**
   * Claim the next generation and return a predicate that stays true only while
   * this generation is the newest one.
   */
  begin(): () => boolean;
  /**
   * Supersede every in-flight generation without starting a new one. Destructive
   * actions call this so a read issued before them cannot commit afterwards —
   * without it, a history list request that started before "Clear history" can
   * resolve after it and put the deleted records back on screen.
   */
  invalidate(): void;
};

export function createGeneration(): Generation {
  let current = 0;
  return {
    begin() {
      const mine = ++current;
      return () => mine === current;
    },
    invalidate() {
      current += 1;
    },
  };
}
