// Not allowed to import tauri, and does not: it goes through the transport module.
import { channel } from './transport/channel.ts';

export const wired = channel;
