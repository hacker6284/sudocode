//! Embedded standard library sources (spec/language.md §9). Each `.sudo`
//! file lives at the repo root under `stdlib/` and is captured verbatim via
//! `include_str!`, so `cargo build` picks up edits to those files
//! automatically — no separate embed/codegen step, no vendoring by
//! downstream consumers of this binary.

pub const REGEX: &str = include_str!("../../../../stdlib/regex.sudo");
pub const STRINGS: &str = include_str!("../../../../stdlib/strings.sudo");
pub const SORTING: &str = include_str!("../../../../stdlib/sorting.sudo");
pub const BIGINT: &str = include_str!("../../../../stdlib/bigint.sudo");

/// Every embedded module name, sorted — used to render "unknown std module"
/// error messages.
pub const NAMES: &[&str] = &["bigint", "regex", "sorting", "strings"];

/// Look up an embedded stdlib module's source by name (the name after
/// `std.`, e.g. `"regex"` for `import std.regex`). `None` if `name` isn't
/// one of the embedded modules.
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "bigint" => Some(BIGINT),
        "regex" => Some(REGEX),
        "sorting" => Some(SORTING),
        "strings" => Some(STRINGS),
        _ => None,
    }
}
