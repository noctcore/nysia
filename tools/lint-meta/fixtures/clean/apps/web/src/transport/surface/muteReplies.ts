// The stand-in for the real mute. A module that builds no terminal is not asked to call it,
// which is the other half of rule (e): it must stay silent about everything else.
export function muteTerminalReplies(parser: unknown): void {
  void parser;
}
