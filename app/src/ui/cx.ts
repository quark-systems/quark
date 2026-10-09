/** Joins class names, skipping falsy ones: cx("a", on && "b"). */
export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}
