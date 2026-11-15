//! The `AGENTS_` environment names, read for one more release.

use std::ffi::OsString;

/// Copies every `AGENTS_X` variable to `KBL_X` unless `KBL_X` is already set,
/// so a deployment that still sets the old names keeps working.
///
/// Call it first thing in `main`, before any thread exists and before the
/// arguments are parsed, because the parser reads the `KBL_` names.
pub fn adopt() {
    let vars: Vec<(String, OsString)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
        .collect();
    for (name, value) in renamed(&vars) {
        // SAFETY: the caller runs this before spawning any thread.
        unsafe { std::env::set_var(name, value) };
    }
}

/// The `KBL_` copies still to make for `vars`.
fn renamed(vars: &[(String, OsString)]) -> Vec<(String, OsString)> {
    vars.iter()
        .filter_map(|(k, v)| {
            let new = format!("KBL_{}", k.strip_prefix("AGENTS_")?);
            (!vars.iter().any(|(other, _)| *other == new)).then(|| (new, v.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests;
