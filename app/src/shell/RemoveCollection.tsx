// Confirming the removal of a collection: its name has to be typed, as deleting a project
// does, so a stray click never loses a collection and the order it holds. The projects in it
// are untouched either way.
import React, { useState } from "react";
import { Dialog } from "radix-ui";
import { api } from "../api";
import { setCollections } from "../store";
import { errText } from "../util";
import { Button, TextInput } from "../ui";

export function RemoveCollection({ id, name, onClose }: { id: string; name: string; onClose: () => void }) {
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const matches = typed.trim() === name;

  const remove = async () => {
    if (!matches) return;
    setBusy(true); setErr(null);
    try {
      await api.deleteCollection(id, name);
      setCollections(await api.collections());
      onClose();
    } catch (e) {
      setErr(errText(e)); setBusy(false);
    }
  };

  return (
    <Dialog.Root open onOpenChange={(open) => { if (!open) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-[var(--scrim)] backdrop-blur-[2px]" />
        <Dialog.Content data-testid="remove-collection"
          className="fixed left-1/2 top-1/3 z-50 w-[min(28rem,calc(100vw-2rem))] -translate-x-1/2 rounded-l bg-popover p-4 text-fg shadow-popover outline-none">
          <Dialog.Title className="m-0 font-sans text-m font-medium">Remove {name}?</Dialog.Title>
          <Dialog.Description className="mt-2 font-sans text-s text-dim">
            The projects in it stay exactly as they are; the collection and the order it holds are lost.
            Type its name to confirm.
          </Dialog.Description>
          <form className="mt-3 flex flex-col gap-3" onSubmit={(e) => { e.preventDefault(); void remove(); }}>
            <TextInput autoFocus value={typed} disabled={busy} aria-label={`Type ${name} to confirm`}
              placeholder={name} data-testid="remove-collection-name"
              onChange={(e) => setTyped(e.target.value)} />
            {err && <div className="text-s text-danger-fg" role="alert">{err}</div>}
            <div className="flex justify-end gap-2">
              <Button kind="quiet" type="button" onClick={onClose}>Cancel</Button>
              <Button kind="danger" type="submit" disabled={!matches || busy} data-testid="remove-collection-confirm">
                Remove collection
              </Button>
            </div>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
