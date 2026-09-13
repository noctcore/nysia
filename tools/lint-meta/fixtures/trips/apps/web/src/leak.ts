// Rule (a): a webview component reaching for Tauri directly instead of going through
// apps/web/src/transport.
import { Channel } from '@tauri-apps/api/core';

export const leaked = Channel;
