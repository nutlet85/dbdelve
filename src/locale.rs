//! The UI language: which locale the menus render in, and where the choice
//! comes from. A stored setting wins; with none, the system locale maps onto
//! the languages DBDelve ships entries for (`SUPPORTED`). rust-i18n's locale
//! is process-global, so setting it here also localises gpui-component's own
//! strings (input context menus, calendars, pagination).
//!
//! What is translated today: the menu layer, the settings panel, the command
//! palette's chrome (its scopes, the field's placeholder, group headers, footer
//! hints, empty states and History's pills), the dialogs that write DDL with
//! the sentences `db::alter` refuses them in, the connection form, the
//! unsaved query tabs' titles, and the query toolbar's target label. What the
//! server said is never translated -- it is marked with `error.server` and
//! shown in its own words, because the server's sentence is the record a bug
//! report needs.

/// The languages this build ships entries for, with their own-language names
/// for the settings row. Kept in display order.
pub(crate) const SUPPORTED: [(&str, &str); 2] = [("en", "English"), ("zh-CN", "简体中文")];

/// Pure, unit-testable. A setting value matches exactly; a system locale
/// folds onto a supported language by prefix, so `zh-Hans-CN` and `zh-TW`
/// both land on `zh-CN`, the only Chinese this build ships.
pub(crate) fn map_system(locale: &str) -> &'static str {
    if locale.starts_with("zh") {
        "zh-CN"
    } else {
        "en"
    }
}

/// Resolve the effective locale and apply it process-wide. `setting` is the
/// stored `language`; `None` means follow the system, and anything the system
/// reports that we do not ship falls back to English.
pub(crate) fn apply(setting: Option<&str>) {
    let resolved = setting.unwrap_or_else(|| {
        sys_locale::get_locale()
            .as_deref()
            .map(map_system)
            .unwrap_or("en")
    });
    rust_i18n::set_locale(resolved);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_system_folds_chinese_variants_onto_zh_cn() {
        assert_eq!(map_system("zh-CN"), "zh-CN");
        assert_eq!(map_system("zh-Hans-CN"), "zh-CN");
    }

    #[test]
    fn map_system_takes_english_and_unknown_to_en() {
        assert_eq!(map_system("en-US"), "en");
        assert_eq!(map_system("fr"), "en");
    }
}
