use gtk4::prelude::*;

pub fn combo_active_id_or(combo: &gtk4::ComboBoxText, default_id: &str) -> String {
    combo
        .active_id()
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_id.to_string())
}

pub fn spin_usize(spin: &gtk4::SpinButton) -> usize {
    spin.value() as usize
}
