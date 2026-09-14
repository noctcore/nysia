// Rule (e): a type-only import builds nothing, so it is obliged to mute nothing.
// `import type` erases entirely — this module cannot construct a terminal even by accident,
// and the first spelling of the rule reported it anyway.
import type { Terminal } from '@xterm/xterm';

export type Surface = Pick<Terminal, 'cols' | 'rows'>;
