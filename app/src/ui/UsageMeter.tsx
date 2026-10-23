// A thin bar for how much of a limit is used, as in Claude's plan usage popover, with an optional
// tick where something happens on the way (the coordinator's compaction threshold).
import { cx } from "./cx";

/** How much of `max` is used. The bar turns yellow once it reaches `mark`. */
export function UsageMeter({ used, max, mark, markLabel, label, testid }: {
  used: number; max: number;
  /** Where the tick goes, in the same unit as `used`. */
  mark?: number; markLabel?: string;
  /** What is measured, for assistive tech: "Context". */
  label: string;
  testid?: string;
}) {
  const at = (n: number) => (max > 0 ? Math.min(100, Math.max(0, (n / max) * 100)) : 0);
  const reached = mark !== undefined && used >= mark;
  return (
    <div role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={max} aria-valuenow={used} data-testid={testid}
      className="relative h-1.5 w-full overflow-visible rounded-full bg-surface-3">
      <div className={cx("h-full rounded-full transition-[width]", reached ? "bg-yellow" : "bg-blue")} style={{ width: `${at(used)}%` }} />
      {mark !== undefined && max > 0 && (
        <div title={markLabel} className="absolute -top-0.5 h-2.5 w-0.5 rounded-full bg-dim" style={{ left: `calc(${at(mark)}% - 1px)` }} />
      )}
    </div>
  );
}
