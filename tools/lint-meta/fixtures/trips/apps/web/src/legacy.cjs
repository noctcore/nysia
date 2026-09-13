// Rule (a), the require() backstop. ESLint's no-restricted-imports does not cover require()
// at all, and no-restricted-modules was removed in ESLint 9 — so this spelling reaches the
// bundle past the layer that is supposed to own JavaScript imports.
const { Channel } = require('@tauri-apps/api/core');

module.exports = { leaked: Channel };
