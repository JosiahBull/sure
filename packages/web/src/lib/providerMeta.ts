// Display helpers for provider kinds. The backend hands us a bare slug (`akahu`, `csv`);
// these turn it into something presentable without the UI needing a hardcoded name table.

/** Human label for a provider kind slug — acronyms upper-cased, otherwise capitalised. */
export const providerLabel = (kind: string): string =>
  kind.length <= 3 ? kind.toUpperCase() : kind.charAt(0).toUpperCase() + kind.slice(1);

/** Two-letter monogram for a provider kind's avatar tile. */
export const providerInitials = (kind: string): string => kind.slice(0, 2).toUpperCase();
