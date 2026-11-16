// A prompt on a worker's screen (a permission request or a menu), as quarkd records it: the
// supervisor reads it off the terminal and asks it as a worker question (quark-supervisor's
// prompt.rs, `Prompt::question`). The worker view shows it as buttons above the message box.

export interface ScreenPrompt {
  /** The screen lines above the choices: what the harness is asking about. */
  context: string[];
  /** The choices, in order; answer with the 1-based number. */
  options: string[];
}

const LEAD = "My terminal shows a prompt waiting for a keypress";

/** The prompt in a question quarkd raised for a worker's screen, or null for any other question. */
export function screenPrompt(question: string): ScreenPrompt | null {
  if (!question.startsWith(LEAD)) return null;
  const fence = question.match(/```\n([\s\S]*?)\n```/);
  const context = fence ? fence[1].split("\n").map((l) => l.trim()).filter(Boolean) : [];
  const at = question.indexOf("\n\nChoices:");
  if (at < 0) return null;
  const options: string[] = [];
  for (const line of question.slice(at + "\n\nChoices:".length).split("\n")) {
    const m = line.match(/^(\d+)\. (.*)$/);
    if (m && Number(m[1]) === options.length + 1) options.push(m[2].trim());
    else if (options.length && !line.trim()) break;
  }
  return options.length ? { context, options } : null;
}

/** The line that names what is asked: the last context line, which is the harness's own question. */
export function promptTitle(p: ScreenPrompt): string {
  return p.context[p.context.length - 1] ?? "The worker's screen is waiting for a choice";
}
