// Rule (e): the same module with the mute applied, which is what the repository ships.
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';

import { muteTerminalReplies } from './muteReplies.ts';

export const build = (): Terminal => {
  const terminal = new Terminal({ cols: 80, rows: 24 });
  muteTerminalReplies(terminal.parser);
  return terminal;
};
