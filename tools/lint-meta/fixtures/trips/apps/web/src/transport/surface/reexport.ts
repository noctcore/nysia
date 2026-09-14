// Rule (e): handing the constructor on under this module's name.
//
// Nothing is built here, so there is nothing to mute — and that is the point. Whoever writes
// `new Terminal()` after importing it from this module names `./reexport`, which is not the
// terminal library, so the rule that matches on specifiers stops seeing the terminal at all.
// Reported outright: the way to hand on a terminal is to build it here and mute it here.
export { Terminal } from '@xterm/xterm';
