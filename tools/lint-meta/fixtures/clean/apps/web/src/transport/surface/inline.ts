// Rule (e): the other spelling of the same thing. `verbatimModuleSyntax` keeps this import
// statement at runtime, so a reader that only checked the clause would call it a value
// binding; the `type` modifier is on each specifier and it erases both of these.
import { type ITerminalOptions, type Terminal } from '@xterm/xterm';

export const cols = (terminal: Terminal, options: ITerminalOptions): number =>
  options.cols ?? terminal.cols;
