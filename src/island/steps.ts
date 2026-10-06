// How a tool call reads on the island: a short line for the ticker and the
// session list, and the full text behind it (tooltip, click to expand).

/** A step as the ticker shows it, and everything it stands for. */
export interface Step {
  text: string;
  /** The command or path in full; the same as `text` when nothing was cut. */
  full: string;
}

/**
 * What each tool is doing, in plain words. The Mac app says it in French
 * (frenchStep()); this build speaks English everywhere else, so the ticker
 * does too. opencode's tools arrive under these names through its plugin.
 */
const TOOL_LABELS: Record<string, string> = {
  Bash: "Run",
  PowerShell: "Run",
  Read: "Read",
  Write: "Write",
  Edit: "Edit",
  MultiEdit: "Edit",
  NotebookEdit: "Edit notebook",
  Glob: "Find files",
  Grep: "Search code",
  LS: "List",
  WebSearch: "Search the web",
  WebFetch: "Open page",
  TodoWrite: "Update to-do list",
  Task: "Subagent",
  Agent: "Subagent",
  Skill: "Skill",
};

/** Fields that say what a tool works on, most telling first. */
const STEP_FIELDS = ["command", "file_path", "notebook_path", "pattern", "path", "query", "url", "description", "skill"] as const;
const PATH_FIELDS = new Set(["file_path", "notebook_path", "path"]);

/** Longer than this, a path keeps its first folders and its name: `~/.local/…/coucou.log`. */
const PATH_MAX = 32;

/** `~` for the home folder, nothing for the project's own folder. */
export function tidyPath(path: string, cwd: string): string {
  const base = cwd.replace(/\/+$/, "");
  if (base && path === base) return ".";
  if (base && path.startsWith(`${base}/`)) return path.slice(base.length + 1);
  return path.replace(/^\/home\/[^/]+(?=\/|$)|^\/root(?=\/|$)/, "~");
}

/** `~/.local/share/coucou/coucou.log` → `~/.local/…/coucou.log`. */
export function shortenPath(path: string): string {
  if (path.length <= PATH_MAX) return path;
  const parts = path.split("/");
  if (parts.length <= 3) return path;
  const head = parts[0] === "" ? `/${parts[1]}` : parts[0];
  return `${head}/…/${parts[parts.length - 1]}`;
}

/**
 * A command as a person would say it: without the `cd <project> &&` an agent
 * puts in front of everything, with `~` and project-relative paths, and long
 * paths cut in the middle. Only for the ticker — the approval card shows the
 * command exactly.
 */
export function tidyCommand(command: string, cwd: string): string {
  const lines = command.trim().split("\n");
  let cmd = lines[0].trim();
  for (;;) {
    const next = cmd.replace(/^cd\s+("[^"]*"|'[^']*'|\S+)\s*(&&|;)\s*/, "");
    if (next === cmd) break;
    cmd = next;
  }
  cmd = cmd.replace(/(^|[\s=:("'])((?:\/(?!\/)|~\/)[^\s'"`;|&)<>]*)/g, (_, lead: string, path: string) =>
    lead + (shortenPath(tidyPath(path, cwd)) || "."),
  );
  return lines.length > 1 ? `${cmd} …` : cmd;
}

/** `Edit · hooks.ts — src/island`: the name first, its folder after. */
function fileStep(path: string, cwd: string): string {
  const tidy = tidyPath(path.replace(/\/+$/, ""), cwd);
  const cut = tidy.lastIndexOf("/");
  if (cut < 0) return tidy;
  const name = tidy.slice(cut + 1);
  const dir = tidy.slice(0, cut) || "/";
  return `${name} — ${shortenPath(dir)}`;
}

export function stepFor(tool: string, input: Record<string, unknown>, cwd = ""): Step {
  // MCP tools: "mcp__server__tool" reads better as "server · tool".
  const mcp = /^mcp__(.+?)__(.+)$/.exec(tool);
  const label = mcp ? `${mcp[1]} · ${mcp[2].replace(/_/g, " ")}` : (TOOL_LABELS[tool] ?? tool);
  const command = typeof input.command === "string" ? input.command.trim() : "";
  // Claude Code and opencode give each command a few words of what it is for:
  // "Build the .deb package" says more than any cut-off command line.
  const description = typeof input.description === "string" ? input.description.trim() : "";
  if (command && description) {
    return { text: `${label} · ${description.split("\n")[0]}`, full: command };
  }
  for (const field of STEP_FIELDS) {
    const value = input[field];
    if (typeof value !== "string" || !value.trim()) continue;
    const raw = value.trim();
    let shown: string;
    if (field === "command") shown = tidyCommand(raw, cwd);
    else if (PATH_FIELDS.has(field)) shown = fileStep(raw, cwd);
    else shown = raw.split("\n")[0];
    const text = `${label} · ${shown.slice(0, 160)}`;
    return { text, full: raw };
  }
  return { text: label, full: label };
}
