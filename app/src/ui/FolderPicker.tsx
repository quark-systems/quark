// A folder chosen with the system dialog. Typing a path is the fallback only where no dialog
// can help: in a plain browser, or when the daemon runs on another machine.
import React from "react";
import { folderPicking, pickFolder } from "../folders";
import { Button } from ".";
import { FieldHint, TextInput } from "./form";

export function FolderPicker({ value, onChange, title, placeholder = "No folder chosen", label }: {
  value: string; onChange: (path: string) => void; title: string; placeholder?: string; label?: string;
}) {
  const how = folderPicking();
  if (!how.ok) {
    return (
      <>
        <TextInput mono value={value} onChange={(e) => onChange(e.target.value)} aria-label={label} placeholder="/path/on/the/daemon/machine" />
        <FieldHint>
          {how.reason === "remote_daemon"
            ? "The daemon runs on another machine, so type a path on that machine."
            : "Type the path; the desktop app opens a folder dialog instead."}
        </FieldHint>
      </>
    );
  }
  const choose = async () => {
    const p = await pickFolder(title, value || undefined);
    if (p) onChange(p);
  };
  return (
    <div className="ui-folder">
      <button type="button" className={"ui-input ui-folder-path" + (value ? "" : " empty")} onClick={choose} aria-label={label} title={value || undefined}>
        {value || placeholder}
      </button>
      <Button onClick={choose}>{value ? "Change…" : "Choose folder…"}</Button>
      {value && <Button kind="quiet" onClick={() => onChange("")}>Clear</Button>}
    </div>
  );
}
