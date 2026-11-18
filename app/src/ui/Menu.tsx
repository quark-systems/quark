// A dropdown menu opened from a button: groups of actions and one-of-a-set choices, with
// Radix's keyboard handling (arrows, type-ahead, Escape returns focus to the trigger).
import React from "react";
import { DropdownMenu } from "radix-ui";
import { cx } from "./cx";

export interface MenuOption { value: string; label: string; icon?: React.ReactNode }
export interface MenuAction { label: string; icon?: React.ReactNode; onSelect: () => void; detail?: React.ReactNode; testid?: string }
/** A switch that stays in the menu: checked shows a tick, and picking it does not close the menu. */
export interface MenuCheckbox { label: string; checked: boolean; onCheckedChange: (checked: boolean) => void; icon?: React.ReactNode; testid?: string }
/** One group of the menu: a choice from a set (shown with a check) or a list of actions. */
export type MenuGroup = { label?: string } & (
  | { value: string; options: MenuOption[]; onChange: (value: string) => void }
  | { checkbox: MenuCheckbox }
  | { actions: MenuAction[] }
);

const ITEM = "flex min-h-8 cursor-pointer select-none items-center gap-2.5 rounded-s px-2 text-control text-fg outline-none data-[highlighted]:bg-selection";
const ICON = "inline-flex w-4 shrink-0 justify-center text-dim";

function Check() {
  return <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg>;
}

/** A menu under `trigger` (a button element, given its own aria-label). */
export function Menu({ trigger, groups, align = "start", label }: {
  trigger: React.ReactElement;
  groups: MenuGroup[];
  align?: "start" | "end";
  /** The menu's accessible name. */
  label: string;
}) {
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>{trigger}</DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content align={align} sideOffset={6} aria-label={label} data-testid="ui-menu"
          className="z-50 min-w-56 rounded-l bg-popover p-1.5 font-sans shadow-popover [animation:popover-open_var(--motion-panel-duration)_var(--motion-pop-ease)]">
          {groups.map((g, i) => (
            <React.Fragment key={i}>
              {i > 0 && <DropdownMenu.Separator className="mx-1 my-1.5 h-px bg-line" />}
              <DropdownMenu.Group>
                {g.label && <DropdownMenu.Label className="px-2 pt-1 pb-1 text-xs font-semibold uppercase tracking-[0.04em] text-faint">{g.label}</DropdownMenu.Label>}
                {"options" in g ? (
                  <DropdownMenu.RadioGroup value={g.value} onValueChange={g.onChange}>
                    {g.options.map((o) => (
                      <DropdownMenu.RadioItem key={o.value} value={o.value} className={ITEM} onSelect={(e) => e.preventDefault()}>
                        <span className={ICON}>{o.icon}</span>
                        <span className="flex-1">{o.label}</span>
                        <DropdownMenu.ItemIndicator className="inline-flex text-fg"><Check /></DropdownMenu.ItemIndicator>
                      </DropdownMenu.RadioItem>
                    ))}
                  </DropdownMenu.RadioGroup>
                ) : "checkbox" in g ? (
                  <DropdownMenu.CheckboxItem checked={g.checkbox.checked} onCheckedChange={g.checkbox.onCheckedChange}
                    className={ITEM} onSelect={(e) => e.preventDefault()} data-testid={g.checkbox.testid}>
                    <span className={ICON}>{g.checkbox.icon}</span>
                    <span className="flex-1">{g.checkbox.label}</span>
                    <DropdownMenu.ItemIndicator className="inline-flex text-fg"><Check /></DropdownMenu.ItemIndicator>
                  </DropdownMenu.CheckboxItem>
                ) : g.actions.map((a) => (
                  <DropdownMenu.Item key={a.label} className={cx(ITEM)} onSelect={a.onSelect} data-testid={a.testid}>
                    <span className={ICON}>{a.icon}</span>
                    <span className="flex-1">{a.label}</span>
                    {a.detail && <span className="text-s text-dim">{a.detail}</span>}
                  </DropdownMenu.Item>
                ))}
              </DropdownMenu.Group>
            </React.Fragment>
          ))}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
