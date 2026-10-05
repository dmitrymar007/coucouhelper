// The models each chat backend offers, shared by the settings window and the
// chat's own model picker.

/** Claude Code takes an alias and resolves it to the current model. */
export const CLI_MODELS: [string, string][] = [
  ["sonnet", "Claude Sonnet"],
  ["opus", "Claude Opus"],
  ["haiku", "Claude Haiku"],
];

/** The Anthropic API takes a model id. */
export const API_MODELS: [string, string][] = [
  ["claude-opus-5", "Claude Opus 5"],
  ["claude-sonnet-5", "Claude Sonnet 5"],
  ["claude-haiku-4-5", "Claude Haiku 4.5"],
];
