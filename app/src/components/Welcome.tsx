// The welcome in a new project's empty coordinator conversation: whether this Mac and the
// project are ready, what the coordinator has to work with (repositories, folder, issues), and
// first messages to send. It goes away once the conversation has its first turn.
import { useEffect, useState } from "react";
import { api, type Issue, type Project, type SetupReport } from "../api";
import { go, href } from "../nav";
import { useLabels } from "../persona";
import { setIssueSeed, loadBeads, useStore } from "../store";
import { Button, ButtonLink, CoordinatorMark, Kbd, cx } from "../ui";
import { MOD } from "../shell/Dock";
import { Icon } from "./transcript/Icon";

type StepState = "done" | "todo" | "waiting";

/** First messages offered on the welcome; each fills the message box for you to edit and send. */
export function suggestions(project: Pick<Project, "goal" | "repos" | "workspace_path">, openIssues: number): { label: string; text: string }[] {
  const out = [{
    label: "Get your bearings",
    text: "Look over this project's repositories and folders and tell me where things stand: what is there, what state it is in, and what you suggest we work on first.",
  }];
  const goal = project.goal?.trim();
  if (goal) out.push({
    label: "Plan the goal",
    text: `Propose a plan for this project's goal: ${goal}\nBreak it into issues I can review before any workers start.`,
  });
  if (openIssues > 0) out.push({
    label: "Pick a first issue",
    text: "Go through the open issues and tell me which one to start with, and why.",
  });
  return out;
}

/** A repository's short name: its own name, or the last part of its URL. */
export function repoName(r: { url: string; name?: string | null }): string {
  return r.name || r.url.replace(/\.git$/, "").split(/[/:]/).filter(Boolean).pop() || r.url;
}

function Step({ state, title, children, action, testid }: {
  state: StepState; title: string; children?: React.ReactNode; action?: React.ReactNode; testid: string;
}) {
  return (
    <li className="flex items-start gap-3 py-3 border-t border-line first:border-t-0" data-testid={testid} data-state={state}>
      <span className={cx("mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full border",
        state === "done" ? "border-green text-green" : state === "waiting" ? "border-yellow text-yellow" : "border-line-2 text-faint")}
        aria-label={state === "done" ? "Done" : state === "waiting" ? "Needs attention" : "To do"} role="img">
        {state === "done" ? <Icon kind="check" /> : state === "waiting" ? <Icon kind="warning" /> : null}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="text-m font-semibold text-fg">{title}</div>
        {children && <div className="text-s text-dim">{children}</div>}
      </div>
      {action && <div className="shrink-0">{action}</div>}
    </li>
  );
}

export function Welcome({ project, onSuggest }: { project: Project; onSuggest: (text: string) => void }) {
  const pid = project.id;
  const l = useLabels(pid);
  const coordinator = l.role("coordinator");
  const beads = useStore((s) => s.beads[pid]);
  const [setup, setSetup] = useState<SetupReport | null>(null);
  const [issues, setIssues] = useState<Issue[] | null>(null);

  useEffect(() => {
    let live = true;
    api.setup().then((r) => { if (live) setSetup(r); }).catch(() => { /* an older daemon has no Setup */ });
    loadBeads(pid).catch(() => {});
    return () => { live = false; };
  }, [pid]);
  const beadsReady = beads?.state === "ready";
  useEffect(() => {
    if (!beadsReady) return;
    let live = true;
    api.issues(pid, "all").then((list) => { if (live) setIssues(list); }).catch(() => {});
    return () => { live = false; };
  }, [pid, beadsReady]);

  const repos = project.repos ?? [];
  const missing = setup ? setup.checks.filter((c) => c.status !== "ok") : [];
  const open = (issues ?? []).filter((i) => i.status !== "closed");
  const newIssue = () => { setIssueSeed({ project_id: pid, text: "" }); go({ name: "issues", project: pid }); };
  const goal = project.goal?.trim();

  return (
    <section className="mx-auto my-6 flex w-full max-w-[640px] flex-col gap-4 rounded-l border border-line-2 bg-surface-1 p-5"
      data-testid="welcome" aria-label={`Welcome to ${project.name}`}>
      <header className="flex items-center gap-3">
        <CoordinatorMark size={32} />
        <div className="flex min-w-0 flex-col">
          <h2 className="m-0 text-[18px] font-semibold text-fg">Welcome to {project.name}</h2>
          {goal && <div className="text-s text-dim">{goal}</div>}
        </div>
      </header>
      <p className="m-0 text-control text-dim">
        This is your conversation with the {coordinator}. It plans the work, starts workers on it, opens PRs and asks you when
        something needs a decision. Here is what it has to work with, and a few ways to start.
      </p>

      <ol className="m-0 list-none p-0">
        <Step testid="welcome-setup" title="Set up this Mac"
          state={!setup ? "todo" : missing.length ? (setup.ready ? "done" : "waiting") : "done"}
          action={setup && missing.length > 0 ? <ButtonLink kind="secondary" href={href({ name: "setup" })}>Open Setup</ButtonLink> : undefined}>
          {!setup ? "Checking tools and sign-ins…"
            : !missing.length ? "Tools and sign-ins are in place."
            : `${missing.length} ${missing.length === 1 ? "thing" : "things"} to look at: ${missing.map((c) => c.title).join(", ")}.`}
        </Step>

        <Step testid="welcome-sources" title="Repositories and folder"
          state={project.status === "failed" ? "waiting" : project.status === "provisioning" ? "todo" : repos.length || project.workspace_path ? "done" : "todo"}
          action={<ButtonLink kind="quiet" href={href({ name: "settings", project: pid })}>Settings</ButtonLink>}>
          {project.status === "provisioning" ? <div>Cloning repositories…</div>
            : project.status === "failed" ? <div>Setting up this project failed{project.status_detail ? `: ${project.status_detail}` : ""}.</div> : null}
          {repos.length > 0 ? (
            <ul className="m-0 mt-1 flex list-none flex-col gap-0.5 p-0" aria-label="Repositories">
              {repos.map((r) => (
                <li key={r.url} className="flex min-w-0 items-baseline gap-2">
                  <span className="font-semibold text-fg">{repoName(r)}</span>
                  <span className="truncate font-mono text-xs text-faint" title={r.url}>{r.url}</span>
                </li>
              ))}
            </ul>
          ) : <div>No repositories yet. The {coordinator} works in the project folder.</div>}
          {project.workspace_path && (
            <div className="mt-1">Folder <span className="font-mono text-xs text-fg">{project.workspace_path}</span></div>
          )}
        </Step>

        <Step testid="welcome-issues" title={open.length ? "Pick an issue to work on" : "Create a first issue"}
          state={!beads || beads.state === "setting_up" ? "todo" : !beadsReady ? "waiting" : open.length ? "done" : "todo"}
          action={!beadsReady
            ? <ButtonLink kind="secondary" href={href({ name: "issues", project: pid })}>{beads?.state === "setting_up" ? "Issues" : "Set up issues"}</ButtonLink>
            : open.length
              ? <ButtonLink kind="secondary" href={href({ name: "issues", project: pid })}>Open Issues</ButtonLink>
              : <Button kind="primary" onClick={newIssue}>New issue</Button>}>
          {!beads ? "Checking issues…"
            : beads.state === "setting_up" ? "Issues are being set up."
            : !beadsReady ? `Issues are not set up yet${beads.detail ? `: ${beads.detail}` : "."}`
            : issues === null ? "Loading issues…"
            : open.length ? `${open.length} open ${open.length === 1 ? "issue" : "issues"}. The ${coordinator} starts workers on ready ones.`
            : `Describe a piece of work and the ${coordinator} drafts it as an issue for you to accept.`}
        </Step>
      </ol>

      <div className="flex flex-col gap-2">
        <div className="text-s font-semibold text-dim">Or ask the {coordinator}</div>
        <div className="flex flex-wrap gap-2" data-testid="welcome-suggestions">
          {suggestions(project, open.length).map((s) => (
            <Button key={s.label} kind="secondary" title={s.text} onClick={() => onSuggest(s.text)}>{s.label}</Button>
          ))}
        </div>
      </div>

      <footer className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-line pt-3 text-s text-faint">
        <span className="inline-flex items-center gap-1.5"><Kbd keys={`${MOD}J`} /> next thing that needs you</span>
        <span className="inline-flex items-center gap-1.5"><Kbd keys={`${MOD}P`} /> go anywhere</span>
        <span className="inline-flex items-center gap-1.5"><Kbd keys="/" /> commands like /status</span>
      </footer>
    </section>
  );
}
