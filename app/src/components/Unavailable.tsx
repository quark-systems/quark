import React from "react";

/** Placeholder for a panel whose daemon endpoint has not landed yet. */
export function Unavailable({ what, endpoint }: { what: string; endpoint: string }) {
  return (
    <div className="unavailable">
      <div className="u-title">{what} is not available from this daemon yet</div>
      <div className="faint mono">{endpoint}</div>
    </div>
  );
}
