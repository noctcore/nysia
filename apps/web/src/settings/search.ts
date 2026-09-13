import { SETTINGS_TREE, type SettingsGroup } from './nav';

/**
 * Filtering the settings nav.
 *
 * Its own module so the two rules below can be stated once and proved without a DOM
 * (D-18) — the group-name behaviour in particular is a decision, not an implementation
 * detail, and it is the kind of thing that quietly regresses when someone simplifies the
 * filter later.
 */

/** Case-insensitive substring match, which is all a nav of this size needs. */
export function matches(label: string, query: string): boolean {
  const needle = query.trim().toLowerCase();
  return needle === '' || label.toLowerCase().includes(needle);
}

/**
 * The tree, filtered.
 *
 * A group whose own name matches keeps all of its entries — someone typing "workflows"
 * wants the section, not nothing — and otherwise a group survives only through the entries
 * that matched. Empty groups are dropped rather than left as bare headings.
 */
export function matchingGroups(query: string): readonly SettingsGroup[] {
  if (query.trim() === '') {
    return SETTINGS_TREE;
  }
  return SETTINGS_TREE.flatMap((group) => {
    if (matches(group.label, query)) {
      return [group];
    }
    const entries = group.entries.filter((entry) => matches(entry.label, query));
    return entries.length === 0 ? [] : [{ ...group, entries }];
  });
}
