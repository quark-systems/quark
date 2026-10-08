// The component catalogue (`#/catalogue`): every shared part in `src/ui` with when to use it,
// its contract and a live example, in the current theme.
import React, { useState } from "react";
import { CATALOGUE } from "../ui/catalogue";

export function Catalogue() {
  const [q, setQ] = useState("");
  const shown = CATALOGUE.filter((e) => !q || (e.name + " " + e.when).toLowerCase().includes(q.toLowerCase()));
  return (
    <>
      <div className="header">
        <h1>Component catalogue</h1>
        <span className="crumb">Shared parts in app/src/ui. Words follow app/GLOSSARY.md.</span>
        <span className="spacer" />
        <input className="cat-search" type="search" aria-label="Search components" placeholder="Search components" value={q} onChange={(e) => setQ(e.target.value)} />
      </div>
      <div className="screen scroll">
        <div className="catalogue">
          {shown.map((e) => (
            <section key={e.name} className="cat-entry" data-testid="catalogue-entry" aria-labelledby={`cat-${e.name}`}>
              <h2 id={`cat-${e.name}`} className="mono">{e.name}</h2>
              <p>{e.when}</p>
              <p className="faint">{e.contract}</p>
              <div className="cat-example">{e.example()}</div>
            </section>
          ))}
          {!shown.length && <div className="empty">No component matches.</div>}
        </div>
      </div>
    </>
  );
}
