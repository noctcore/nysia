// An astral char literal is two UTF-16 code units. A pattern that matches one leaves the
// closing quote to pair with the comma that follows, and the orphaned double quote then
// opens a string that runs to the next one in the file — erasing every import between.
//
// This case lives in its own file on purpose: a later quote anywhere below would close the
// runaway string early and rescue the import, which is exactly how a first attempt at this
// fixture passed while the defect was still present.
const ASTRAL: [char; 2] = ['🔥','"'];

use ::tauri::Builder;
