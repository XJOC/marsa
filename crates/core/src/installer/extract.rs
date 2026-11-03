//! Unpacks a verified file (`tar.gz`, `zip`, or `raw`) into a temp folder by
//! the recipe's rules (`strip_components`, `into`). Refuses entries that would
//! write outside the folder, unsafe links, and archives over the size limits.
//!
//! Filled in by task M2-03.
