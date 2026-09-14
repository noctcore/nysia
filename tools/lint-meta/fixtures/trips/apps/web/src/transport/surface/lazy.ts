// Rule (e): the renderer loaded lazily, which is an ordinary way to split a bundle and was
// silent for exactly as long as the rule read static imports alone. What a dynamic import
// hands over is the whole module, so there is no name written down to scope the obligation
// to — and a terminal that arrives late answers the queries it parses like any other.
export const build = async (): Promise<unknown> => {
  const { Terminal } = await import('@xterm/xterm');
  return new Terminal({ cols: 80, rows: 24 });
};
