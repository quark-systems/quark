// 16px outline glyphs drawn for Quark, one per kind of step plus a few controls.
const PATHS: Record<string, string> = {
  read: "M4 2h5l3 3v9H4zM9 2v3h3M6 8h4M6 10.5h4",
  edit: "M10.5 2.5l3 3L6 13H3v-3zM9 4l3 3",
  write: "M4 2h5l3 3v9H4zM9 2v3h3M8 7.5v4M6 9.5h4",
  shell: "M2 3h12v10H2zM4.5 6.5l2 1.5-2 1.5M8 10h3.5",
  search: "M7 3a4 4 0 1 1 0 8 4 4 0 0 1 0-8zM10 10l3.5 3.5",
  web: "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12",
  agent: "M4 5h8v7H4zM8 2v3M6 8h.01M10 8h.01M6.5 10.5h3",
  plan: "M6 4h7M6 8h7M6 12h7M3 4h.01M3 8h.01M3 12h.01",
  other: "M8 5.5v5M5.5 8h5",
  thinking: "M8 2.5a4 4 0 0 1 2.5 7.1V11h-5V9.6A4 4 0 0 1 8 2.5zM6 13.5h4",
  decision: "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM6.3 6.3a1.8 1.8 0 1 1 2.4 1.7c-.5.2-.7.6-.7 1.1v.4M8 11.5h.01",
  check: "M3.5 8.5l3 3 6-7",
  cross: "M4.5 4.5l7 7M11.5 4.5l-7 7",
  copy: "M5.5 5.5h7v8h-7zM3.5 10.5v-8h7",
  "arrow-down": "M8 3v10M4 9l4 4 4-4",
  "chevron-right": "M6 4l4 4-4 4",
  "chevron-down": "M4 6l4 4 4-4",
};

export function Icon({ kind }: { kind: string }) {
  return (
    <svg className={"tx-icon tx-icon-" + kind} viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <path d={PATHS[kind] ?? PATHS.other} fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
