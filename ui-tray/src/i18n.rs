#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Lang {
    ZhCn,
    En,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum K {
    OpenControlPanel,
    ReloadConfig,
    StartAll,
    StopAll,
    StartRelay,
    StopRelay,
    StartWatch,
    StopWatch,
    StartApply,
    StopApply,
    Quit,
    TooltipTitle,
    TooltipStatusLine,
    TooltipHint,
}

pub fn detect_lang_from_env() -> Lang {
    let v = std::env::var("LC_ALL")
        .ok()
        .or_else(|| std::env::var("LC_MESSAGES").ok())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default();
    let v = v.to_lowercase();
    if v.starts_with("zh") {
        Lang::ZhCn
    } else {
        Lang::En
    }
}

pub fn parse_lang_id(id: &str) -> Option<Lang> {
    match id {
        "zh-cn" | "zh_cn" | "zh" => Some(Lang::ZhCn),
        "en" | "en-us" | "en_us" => Some(Lang::En),
        _ => None,
    }
}

pub fn t(lang: Lang, k: K) -> &'static str {
    match (lang, k) {
        (Lang::ZhCn, K::OpenControlPanel) => "打开控制面板",
        (Lang::En, K::OpenControlPanel) => "Open Control Panel",

        (Lang::ZhCn, K::ReloadConfig) => "重载配置",
        (Lang::En, K::ReloadConfig) => "Reload config",

        (Lang::ZhCn, K::StartAll) => "一键启动（全部）",
        (Lang::En, K::StartAll) => "Start all",

        (Lang::ZhCn, K::StopAll) => "一键停止（全部）",
        (Lang::En, K::StopAll) => "Stop all",

        (Lang::ZhCn, K::StartRelay) => "启动 relay",
        (Lang::En, K::StartRelay) => "Start relay",
        (Lang::ZhCn, K::StopRelay) => "停止 relay",
        (Lang::En, K::StopRelay) => "Stop relay",

        (Lang::ZhCn, K::StartWatch) => "启动 wl-watch",
        (Lang::En, K::StartWatch) => "Start wl-watch",
        (Lang::ZhCn, K::StopWatch) => "停止 wl-watch",
        (Lang::En, K::StopWatch) => "Stop wl-watch",

        (Lang::ZhCn, K::StartApply) => "启动 wl-apply",
        (Lang::En, K::StartApply) => "Start wl-apply",
        (Lang::ZhCn, K::StopApply) => "停止 wl-apply",
        (Lang::En, K::StopApply) => "Stop wl-apply",

        (Lang::ZhCn, K::Quit) => "退出",
        (Lang::En, K::Quit) => "Quit",

        (Lang::ZhCn, K::TooltipTitle) => "ClipRelay",
        (Lang::En, K::TooltipTitle) => "ClipRelay",
        (Lang::ZhCn, K::TooltipStatusLine) => "状态",
        (Lang::En, K::TooltipStatusLine) => "Status",
        (Lang::ZhCn, K::TooltipHint) => "提示：GNOME 可能需要 AppIndicator 扩展才能显示托盘。",
        (Lang::En, K::TooltipHint) => "Note: GNOME may require an AppIndicator extension to show the tray.",
    }
}
