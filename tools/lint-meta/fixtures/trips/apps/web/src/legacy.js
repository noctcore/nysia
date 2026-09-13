// Rule (a), the JavaScript backstop. ESLint parses this properly now that its ban blocks
// match .js as well as .ts; lint-meta still has to see it for the files ESLint ignores.
// The specifier is not on the `import` line, which is what used to slip past both layers.
import {
  Channel,
} from '@tauri-apps/api/core';

export const leaked = Channel;
