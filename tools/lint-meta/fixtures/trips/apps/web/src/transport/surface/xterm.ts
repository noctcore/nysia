// Rule (e): the module that builds the terminal, with the one line that mutes it deleted.
// This is exactly what the real file looked like after the deletion `pnpm test` did not
// notice — the import is still there, the terminal is still built, and nothing answers for
// the fact that it now answers every query it parses.
//
// The mute is named in a comment below, deliberately: muteTerminalReplies appears in prose in
// the real file too, and a rule that searched the text would count this line and pass.
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';

export const build = (): Terminal => new Terminal({ cols: 80, rows: 24 });
