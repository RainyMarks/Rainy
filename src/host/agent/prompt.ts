/** System prompt assembly. */

/** Inputs of one system prompt. */
export interface PromptInputs {
  cwd: string
  platform: 'win32' | 'linux'
  /** Names of the tools the model can call. */
  tools: readonly string[]
  /** Instructions published by connected MCP servers, by server name. */
  mcpInstructions: readonly { server: string; text: string }[]
  /** Project directories besides the working directory. */
  extraRoots: readonly string[]
  /** Skill descriptor section from `skillsSection`. */
  skills: string
  /** The user's global prompt from Settings. */
  globalPrompt: string
}

const PERSONA = 'You are RainyAgent, a concise coding assistant. Follow the user and applicable project instructions. '
  + 'Inspect relevant files before editing. Use the available file and shell tools to complete the task. '
  + 'Check command results and report what was actually verified. Keep tool output bounded; use rg and targeted line ranges. '
  + 'Continue from compacted checkpoints without repeating finished work.'

const FILE_REFERENCES = 'Tokens prefixed with @ are paths the user explicitly referenced. Relative paths resolve from the workspace root; '
  + 'absolute paths identify files or directories on the host. A trailing slash marks a directory: list it when its contents matter. '
  + 'Anything else is a file: use the read tool when its contents are needed, and do not claim to have inspected it before reading. '
  + '@"..." quotes a path containing spaces.'

const TOOL_NOTES: Readonly<Record<string, string>> = {
  bash: 'Check the [exit code: N] marker on every bash result; investigate failures before moving on.',
  pwsh: 'Non-zero exits are reported as `[exit code: N]` markers; investigate failures before moving on. On Windows a killed process '
    + 'settles as `[exit code: 1]` without a signal marker; treat a bare exit 1 after an interruption as a termination, not a command failure.',
  read: 'Use the read tool — not shell commands like cat — to inspect text files. Use offset and limit to continue reading large files.',
  write: 'Read an existing file before overwriting it with write and prefer edit for targeted changes.',
  edit: 'Read a file before editing it, unless you just created or edited it in this session.',
}
const TOOL_NOTE_ORDER = ['bash', 'pwsh', 'read', 'write', 'edit'] as const

/** Bytes of one MCP server's instructions kept in the prompt. */
const MCP_INSTRUCTION_BYTES = 8192

/**
 * Build the system prompt. Sections appear in a fixed order so the prompt stays byte-identical across requests
 * of a chat, which keeps provider prompt caches warm.
 * @param inputs Prompt inputs.
 * @returns The prompt text.
 */
export function buildSystemPrompt(inputs: PromptInputs): string {
  const sections: string[] = [PERSONA]
  if (inputs.tools.includes('read')) sections.push(FILE_REFERENCES)
  for (const name of TOOL_NOTE_ORDER) if (inputs.tools.includes(name)) sections.push(TOOL_NOTES[name]!)
  for (const { server, text } of inputs.mcpInstructions) {
    if (text.trim() === '') continue
    const bytes = Buffer.from(text.trim(), 'utf8')
    sections.push(`### MCP server: ${server}\n\n${bytes.length > MCP_INSTRUCTION_BYTES ? bytes.subarray(0, MCP_INSTRUCTION_BYTES).toString('utf8') : text.trim()}`)
  }
  if (inputs.extraRoots.length > 0) {
    sections.push(`Additional project directories: ${JSON.stringify(inputs.extraRoots)}. Use their absolute paths; the session working directory stays unchanged.`)
  }
  if (inputs.skills !== '') sections.push(inputs.skills)
  if (inputs.globalPrompt.trim() !== '') sections.push(inputs.globalPrompt.trim())
  sections.push(inputs.platform === 'win32'
    ? `Working directory: ${inputs.cwd}. Commands execute in PowerShell on Windows.`
    : `Working directory: ${inputs.cwd}. Commands execute in Bash on Linux (WSL).`)
  return sections.join('\n\n')
}
