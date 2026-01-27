use gtk4::prelude::*;

use std::cell::Cell;
use std::rc::Rc;

use crate::i18n::{help_text, image_mode_hint_text, populate_image_mode_combo, t, Lang, K};

use super::constants::{
    DEFAULT_IMAGE_MODE_ID, LANG_AUTO_ID, PAGE_ACTIVITY, PAGE_CONTROL, PAGE_HELP,
};

#[derive(Clone)]
pub struct ApplyLangCtx {
    pub window: gtk4::ApplicationWindow,
    pub stack: gtk4::Stack,

    pub language_combo: gtk4::ComboBoxText,
    pub image_mode_combo: gtk4::ComboBoxText,

    pub suppress_lang_combo: Rc<Cell<bool>>,
    pub suppress_mode_combo: Rc<Cell<bool>>,

    pub mode_hint: gtk4::Label,
    pub help_buf: gtk4::TextBuffer,

    pub clear_logs_btn: gtk4::Button,
    pub clear_history_btn: gtk4::Button,
    pub reload_btn: gtk4::Button,

    // Activity sub-tabs
    pub tab_history_lbl: gtk4::Label,
    pub tab_app_logs_lbl: gtk4::Label,
    pub tab_clipboard_logs_lbl: gtk4::Label,

    // Labels
    pub lbl_relay: gtk4::Label,
    pub lbl_room: gtk4::Label,
    pub lbl_max_text: gtk4::Label,
    pub lbl_max_img: gtk4::Label,
    pub lbl_max_file: gtk4::Label,
    pub lbl_x11_poll: gtk4::Label,
    pub lbl_img_mode: gtk4::Label,
    pub lbl_lang: gtk4::Label,

    // Services / status labels
    pub lbl_relay_tcp: gtk4::Label,

    // Service buttons
    pub start_relay: gtk4::Button,
    pub stop_relay: gtk4::Button,
    pub start_watch: gtk4::Button,
    pub stop_watch: gtk4::Button,
    pub start_apply: gtk4::Button,
    pub stop_apply: gtk4::Button,
    pub start_x11_sync: gtk4::Button,
    pub stop_x11_sync: gtk4::Button,
    pub start_all: gtk4::Button,
    pub stop_all: gtk4::Button,

    // Diagnostics buttons
    pub send_test_text: gtk4::Button,
    pub send_test_image: gtk4::Button,
    pub send_test_file: gtk4::Button,
    pub show_clip_types: gtk4::Button,

    // Status strings depend on language.
    pub update_services_ui: Rc<dyn Fn()>,
}

pub fn make_apply_lang(ctx: ApplyLangCtx) -> Rc<dyn Fn(Lang)> {
    Rc::new(move |lang: Lang| {
        ctx.window.set_title(Some(t(lang, K::WindowTitle)));

        // Stack tab titles
        // NOTE: `Stack::page(&child)` will assert if `child` isn't inside the stack yet.
        // We may call `apply_lang` before pages are added (during UI construction), so guard it.
        if let Some(w) = ctx.stack.child_by_name(PAGE_CONTROL) {
            ctx.stack.page(&w).set_title(t(lang, K::TabControl));
        }
        if let Some(w) = ctx.stack.child_by_name(PAGE_HELP) {
            ctx.stack.page(&w).set_title(t(lang, K::TabHelp));
        }
        if let Some(w) = ctx.stack.child_by_name(PAGE_ACTIVITY) {
            ctx.stack.page(&w).set_title(t(lang, K::TabActivity));
        }

        // Labels
        ctx.lbl_relay.set_text(t(lang, K::LabelRelay));
        ctx.lbl_room.set_text(t(lang, K::LabelRoom));
        ctx.lbl_max_text.set_text(t(lang, K::LabelMaxTextBytes));
        ctx.lbl_max_img.set_text(t(lang, K::LabelMaxImageBytes));
        ctx.lbl_max_file.set_text(t(lang, K::LabelMaxFileBytes));
        ctx.lbl_x11_poll.set_text(t(lang, K::LabelX11PollIntervalMs));
        ctx.lbl_img_mode.set_text(t(lang, K::LabelImageMode));
        ctx.lbl_lang.set_text(t(lang, K::LabelLanguage));
        ctx.lbl_relay_tcp.set_text(t(lang, K::LabelRelayTcp));

        // Buttons
        ctx.start_relay.set_label(t(lang, K::BtnStartRelay));
        ctx.stop_relay.set_label(t(lang, K::BtnStopRelay));
        ctx.start_watch.set_label(t(lang, K::BtnStartWatch));
        ctx.stop_watch.set_label(t(lang, K::BtnStopWatch));
        ctx.start_apply.set_label(t(lang, K::BtnStartApply));
        ctx.stop_apply.set_label(t(lang, K::BtnStopApply));
        ctx.start_x11_sync.set_label(t(lang, K::BtnStartX11Sync));
        ctx.stop_x11_sync.set_label(t(lang, K::BtnStopX11Sync));
        ctx.start_all.set_label(t(lang, K::BtnStartAll));
        ctx.stop_all.set_label(t(lang, K::BtnStopAll));

        ctx.send_test_text.set_label(t(lang, K::BtnSendTestText));
        ctx.send_test_image.set_label(t(lang, K::BtnSendTestImage));
        ctx.send_test_file.set_label(t(lang, K::BtnSendTestFile));
        ctx.show_clip_types.set_label(t(lang, K::BtnShowClipTypes));

        ctx.clear_logs_btn.set_label(t(lang, K::BtnClearLogs));
        ctx.clear_history_btn.set_label(t(lang, K::BtnClearHistory));
        ctx.reload_btn.set_label(t(lang, K::BtnReloadConfig));

        ctx.tab_history_lbl.set_text(t(lang, K::SubTabHistory));
        ctx.tab_app_logs_lbl.set_text(t(lang, K::SubTabAppLogs));
        ctx.tab_clipboard_logs_lbl
            .set_text(t(lang, K::SubTabClipboardLogs));

        // Language combo labels (keep active id)
        ctx.suppress_lang_combo.set(true);
        let active_lang = ctx
            .language_combo
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| LANG_AUTO_ID.to_string());
        ctx.language_combo.remove_all();
        ctx.language_combo
            .append(Some(LANG_AUTO_ID), t(lang, K::LangAuto));
        ctx.language_combo
            .append(Some("zh-cn"), t(lang, K::LangZhCn));
        ctx.language_combo.append(Some("en"), t(lang, K::LangEn));
        ctx.language_combo.set_active_id(Some(&active_lang));
        ctx.suppress_lang_combo.set(false);

        // Image mode combo labels (keep active id)
        ctx.suppress_mode_combo.set(true);
        populate_image_mode_combo(&ctx.image_mode_combo, lang, None);
        ctx.suppress_mode_combo.set(false);

        // Help text
        ctx.help_buf.set_text(&help_text(lang));

        // Mode hint
        let mode = ctx
            .image_mode_combo
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| DEFAULT_IMAGE_MODE_ID.to_string());
        ctx.mode_hint.set_text(image_mode_hint_text(lang, &mode));

        // Status strings depend on language too.
        (ctx.update_services_ui)();
    })
}
