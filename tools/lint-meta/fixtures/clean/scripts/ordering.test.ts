// Rule (e): the case that made the rule wrong, kept as the proof that it no longer is.
//
// This is the shape of the executed proof of xterm's handler ordering — reach past the
// package for the parser alone and construct one. It is a value import of a real class from
// a build of the library, and it builds no terminal, so nothing here can answer a query and
// nothing here has a mute to call. A rule that asked "does this module reach the library"
// reported it, and lint-meta has no suppression mechanism, so there was no honest way out.
import { EscapeSequenceParser } from '@xterm/xterm/src/common/parser/EscapeSequenceParser';

export const lastRegisteredWins = (): boolean => {
  const parser = new EscapeSequenceParser();
  let reachedTheBuiltIn = true;
  parser.registerCsiHandler({ final: 'c' }, () => {
    reachedTheBuiltIn = false;
    return true;
  });
  return !reachedTheBuiltIn;
};
