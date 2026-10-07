// A small line chart of one series, with the latest value beside it. The full numbers are in the title.
import React from "react";
import { sparkPoints } from "./format";

const W = 160, H = 32;

export function Spark({ label, values, max, now, color = "var(--accent)", testId }: {
  label: string; values: number[]; max?: number; now: string; color?: string; testId?: string;
}) {
  const pts = sparkPoints(values, W, H, max);
  return (
    <div className="hs-spark" data-testid={testId}>
      <div className="hs-spark-head"><span className="hs-label">{label}</span><span className="hs-now">{now}</span></div>
      {values.length > 1
        ? <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label={`${label} over the window`}>
            <polyline points={`0,${H} ${pts} ${W},${H}`} fill={color} fillOpacity={0.12} stroke="none" />
            <polyline points={pts} fill="none" stroke={color} strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
          </svg>
        : <div className="hs-spark-empty faint">{values.length ? "one sample so far" : "no samples"}</div>}
    </div>
  );
}
