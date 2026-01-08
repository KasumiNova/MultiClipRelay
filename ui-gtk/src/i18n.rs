use gtk4::prelude::*;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Lang {
    ZhCn,
    En,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum K {
    WindowTitle,
    TabControl,
    TabLogs,
    TabHelp,
    TabHistory,
    SectionConfig,
    SectionServices,
    SectionTest,
    LabelRelay,
    LabelRoom,
    LabelMaxTextBytes,
    LabelMaxImageBytes,
    LabelMaxFileBytes,
    LabelX11PollIntervalMs,
    LabelImageMode,
    LabelLanguage,
    BtnStartRelay,
    BtnStopRelay,
    BtnStartWatch,
    BtnStopWatch,
    BtnStartApply,
    BtnStopApply,
    BtnStartX11Sync,
    BtnStopX11Sync,
    BtnSendTestText,
    BtnSendTestImage,
    BtnSendTestFile,
    BtnShowClipTypes,
    BtnWlClipboardLogs,
    BtnClearLogs,
    BtnClearHistory,
    ChooseImageTitle,
    ImageFilterName,
    ChooseFileTitle,
    FileFilterName,
    LangAuto,
    LangZhCn,
    LangEn,
    ModeForcePng,
    ModeMulti,
    ModePassthrough,
    ModeSpoofPng,

    BtnStartAll,
    BtnStopAll,
    BtnReloadConfig,
    StatusRunning,
    StatusStopped,
    StatusConnected,
    StatusDisconnected,
    StatusChecking,

    LabelRelayTcp,

    WindowWlClipboardLogs,

    HistoryEmptyHint,
}

pub fn detect_lang_from_env() -> Lang {
    // Extremely small i18n: decide based on LANG / LC_ALL / LC_MESSAGES.
    // Examples: zh_CN.UTF-8, zh_CN, en_US.UTF-8
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
        (Lang::ZhCn, K::WindowTitle) => "MultiClipRelay（控制面板）",
        (Lang::En, K::WindowTitle) => "MultiClipRelay (Control Panel)",

        (Lang::ZhCn, K::TabControl) => "控制",
        (Lang::En, K::TabControl) => "Control",
        (Lang::ZhCn, K::TabLogs) => "日志",
        (Lang::En, K::TabLogs) => "Logs",
        (Lang::ZhCn, K::TabHelp) => "说明",
        (Lang::En, K::TabHelp) => "Help",
        (Lang::ZhCn, K::TabHistory) => "历史",
        (Lang::En, K::TabHistory) => "History",

        (Lang::ZhCn, K::SectionConfig) => "配置",
        (Lang::En, K::SectionConfig) => "Config",
        (Lang::ZhCn, K::SectionServices) => "服务",
        (Lang::En, K::SectionServices) => "Services",
        (Lang::ZhCn, K::SectionTest) => "测试 / 诊断",
        (Lang::En, K::SectionTest) => "Test / Diagnostics",

        (Lang::ZhCn, K::LabelRelay) => "Relay",
        (Lang::En, K::LabelRelay) => "Relay",
        (Lang::ZhCn, K::LabelRoom) => "房间",
        (Lang::En, K::LabelRoom) => "Room",
        (Lang::ZhCn, K::LabelMaxTextBytes) => "文本大小上限（bytes）",
        (Lang::En, K::LabelMaxTextBytes) => "Max text bytes",
        (Lang::ZhCn, K::LabelMaxImageBytes) => "图片大小上限（bytes）",
        (Lang::En, K::LabelMaxImageBytes) => "Max image bytes",
        (Lang::ZhCn, K::LabelMaxFileBytes) => "文件大小上限（bytes）",
        (Lang::En, K::LabelMaxFileBytes) => "Max file bytes",
        (Lang::ZhCn, K::LabelX11PollIntervalMs) => "X11 轮询间隔（ms）",
        (Lang::En, K::LabelX11PollIntervalMs) => "X11 poll interval (ms)",
        (Lang::ZhCn, K::LabelImageMode) => "图片模式",
        (Lang::En, K::LabelImageMode) => "Image mode",
        (Lang::ZhCn, K::LabelLanguage) => "语言",
        (Lang::En, K::LabelLanguage) => "Language",

        (Lang::ZhCn, K::BtnStartRelay) => "启动 relay",
        (Lang::En, K::BtnStartRelay) => "Start relay",
        (Lang::ZhCn, K::BtnStopRelay) => "停止 relay",
        (Lang::En, K::BtnStopRelay) => "Stop relay",

        (Lang::ZhCn, K::BtnStartWatch) => "启动 wl-watch",
        (Lang::En, K::BtnStartWatch) => "Start wl-watch",
        (Lang::ZhCn, K::BtnStopWatch) => "停止 wl-watch",
        (Lang::En, K::BtnStopWatch) => "Stop wl-watch",

        (Lang::ZhCn, K::BtnStartApply) => "启动 wl-apply",
        (Lang::En, K::BtnStartApply) => "Start wl-apply",
        (Lang::ZhCn, K::BtnStopApply) => "停止 wl-apply",
        (Lang::En, K::BtnStopApply) => "Stop wl-apply",

        (Lang::ZhCn, K::BtnStartX11Sync) => "启动 x11-sync",
        (Lang::En, K::BtnStartX11Sync) => "Start x11-sync",
        (Lang::ZhCn, K::BtnStopX11Sync) => "停止 x11-sync",
        (Lang::En, K::BtnStopX11Sync) => "Stop x11-sync",

        (Lang::ZhCn, K::BtnSendTestText) => "发送测试文本",
        (Lang::En, K::BtnSendTestText) => "Send test text",
        (Lang::ZhCn, K::BtnSendTestImage) => "发送测试图片",
        (Lang::En, K::BtnSendTestImage) => "Send test image",
        (Lang::ZhCn, K::BtnSendTestFile) => "发送测试文件",
        (Lang::En, K::BtnSendTestFile) => "Send test file",
        (Lang::ZhCn, K::BtnShowClipTypes) => "查看剪贴板类型",
        (Lang::En, K::BtnShowClipTypes) => "Show clipboard types",
        (Lang::ZhCn, K::BtnWlClipboardLogs) => "wl-clipboard 日志",
        (Lang::En, K::BtnWlClipboardLogs) => "wl-clipboard logs",
        (Lang::ZhCn, K::BtnClearLogs) => "清空日志",
        (Lang::En, K::BtnClearLogs) => "Clear logs",
        (Lang::ZhCn, K::BtnClearHistory) => "清空历史",
        (Lang::En, K::BtnClearHistory) => "Clear history",

        (Lang::ZhCn, K::ChooseImageTitle) => "选择图片文件",
        (Lang::En, K::ChooseImageTitle) => "Choose an image file",
        (Lang::ZhCn, K::ImageFilterName) => "图片（png/jpg/webp/gif）",
        (Lang::En, K::ImageFilterName) => "Images (png/jpg/webp/gif)",

        (Lang::ZhCn, K::ChooseFileTitle) => "选择文件",
        (Lang::En, K::ChooseFileTitle) => "Choose a file",
        (Lang::ZhCn, K::FileFilterName) => "任意文件",
        (Lang::En, K::FileFilterName) => "Any file",

        (Lang::ZhCn, K::LangAuto) => "自动（跟随系统）",
        (Lang::En, K::LangAuto) => "Auto (system)",
        (Lang::ZhCn, K::LangZhCn) => "中文（简体）",
        (Lang::En, K::LangZhCn) => "Chinese (Simplified)",
        (Lang::ZhCn, K::LangEn) => "English",
        (Lang::En, K::LangEn) => "English",

        (Lang::ZhCn, K::ModeForcePng) => "强制 PNG（推荐）",
        (Lang::En, K::ModeForcePng) => "Force PNG (recommended)",
        (Lang::ZhCn, K::ModeMulti) => "多 MIME（原格式 + PNG 兜底）",
        (Lang::En, K::ModeMulti) => "Multi-MIME (original + png)",
        (Lang::ZhCn, K::ModePassthrough) => "直通（仅原格式）",
        (Lang::En, K::ModePassthrough) => "Passthrough (original only)",
        (Lang::ZhCn, K::ModeSpoofPng) => "伪装 PNG（实验/高风险）",
        (Lang::En, K::ModeSpoofPng) => "Spoof PNG (experimental / risky)",

        (Lang::ZhCn, K::BtnStartAll) => "全部启动",
        (Lang::En, K::BtnStartAll) => "Start all",
        (Lang::ZhCn, K::BtnStopAll) => "全部停止",
        (Lang::En, K::BtnStopAll) => "Stop all",

        (Lang::ZhCn, K::BtnReloadConfig) => "重新加载配置",
        (Lang::En, K::BtnReloadConfig) => "Reload config",

        (Lang::ZhCn, K::StatusRunning) => "运行中",
        (Lang::En, K::StatusRunning) => "Running",
        (Lang::ZhCn, K::StatusStopped) => "已停止",
        (Lang::En, K::StatusStopped) => "Stopped",

        (Lang::ZhCn, K::StatusConnected) => "已连接",
        (Lang::En, K::StatusConnected) => "Connected",
        (Lang::ZhCn, K::StatusDisconnected) => "未连接",
        (Lang::En, K::StatusDisconnected) => "Disconnected",
        (Lang::ZhCn, K::StatusChecking) => "检测中…",
        (Lang::En, K::StatusChecking) => "Checking…",

        (Lang::ZhCn, K::LabelRelayTcp) => "Relay 连接（TCP）",
        (Lang::En, K::LabelRelayTcp) => "Relay TCP",

        (Lang::ZhCn, K::WindowWlClipboardLogs) => "wl-clipboard 日志（systemd）",
        (Lang::En, K::WindowWlClipboardLogs) => "wl-clipboard logs (systemd)",

        (Lang::ZhCn, K::HistoryEmptyHint) => {
            "（暂无同步记录：开始 wl-watch / wl-apply 后，这里会显示最近的发送/接收历史）"
        }
        (Lang::En, K::HistoryEmptyHint) => {
            "(No history yet. Start wl-watch / wl-apply to see recent send/receive events.)"
        }
    }
}

pub fn populate_image_mode_combo(combo: &gtk4::ComboBoxText, lang: Lang, active_id: Option<&str>) {
    let keep = active_id
        .map(|s| s.to_string())
        .or_else(|| combo.active_id().map(|s| s.to_string()));
    combo.remove_all();
    combo.append(Some("force-png"), t(lang, K::ModeForcePng));
    combo.append(Some("multi"), t(lang, K::ModeMulti));
    combo.append(Some("passthrough"), t(lang, K::ModePassthrough));
    combo.append(Some("spoof-png"), t(lang, K::ModeSpoofPng));
    if let Some(id) = keep {
        combo.set_active_id(Some(&id));
    }
}

pub fn image_mode_hint_text(lang: Lang, mode_id: &str) -> &'static str {
    match (lang, mode_id) {
        (Lang::ZhCn, "force-png") => "最稳的兼容模式：无论收到/发送什么图片，最终都按 image/png 提供。适合 Electron/Qt 等粘贴兼容性优先的场景。",
        (Lang::En, "force-png") => "Most compatible: always provide images as image/png. Recommended for Electron/Qt and general paste reliability.",

        (Lang::ZhCn, "multi") => "提供“原始 MIME + PNG 兜底”两种表示，理论上最佳。但已知某些 Electron 应用在遇到 image/jpeg 等 offer 时可能卡死（尤其是跨设备传来的 jpeg）。如遇卡死请改用 force-png 或 spoof-png。",
        (Lang::En, "multi") => "Offer original MIME plus a PNG fallback. In theory best-of-both. Known issue: some Electron apps may freeze when image/jpeg offers exist (especially relayed JPEG). Use force-png or spoof-png if that happens.",

        (Lang::ZhCn, "passthrough") => "保持原始 MIME（jpeg/webp/gif/png）。对支持这些格式的应用更“原汁原味”，但粘贴兼容性不如 PNG。",
        (Lang::En, "passthrough") => "Keep original MIME (jpeg/webp/gif/png). More faithful for apps that support it, but paste compatibility may be worse than PNG.",

        (Lang::ZhCn, "spoof-png") => "实验性绕过：对外宣称 image/png，但实际字节仍是原格式。已知对 jpg 在某些应用里能避免卡死，但这是“骗过客户端”的做法，可能导致崩溃/花屏/安全风险，请谨慎。",
        (Lang::En, "spoof-png") => "Experimental workaround: claim image/png but serve original bytes. Can avoid freezes for some apps with JPG, but it is intentionally lying and may cause crashes/garbled images/security issues.",

        (Lang::ZhCn, _) => "",
        (Lang::En, _) => "",
    }
}

pub fn help_text(lang: Lang) -> String {
    match lang {
        Lang::ZhCn => {
            "图片模式说明（重要）：\n\n\
1) 强制 PNG（force-png，推荐）\n\
   - 收到/发送图片时都转换成 image/png。\n\
   - 这是目前日用兼容性最好的模式。\n\n\
2) 多 MIME（multi）\n\
   - 尝试同时提供“原始格式（如 image/jpeg）”和“PNG 兜底”。\n\
   - 理论上最优，但已知：通过本工具转发的 jpeg MIME 可能会导致部分 Electron 应用在粘贴时卡死。\n\
   - 如果你遇到卡死：请切回 force-png，或尝试 spoof-png。\n\n\
3) 直通（passthrough）\n\
   - 不做转换，保持原始 MIME（jpeg/webp/gif/png）。\n\
   - 适合你明确知道目标应用支持该格式的场景。\n\n\
4) 伪装 PNG（spoof-png，实验/高风险）\n\
   - 对外宣称是 image/png，但实际 payload 仍是原始 bytes。\n\
   - 已知对 jpg 在一些 Electron 场景可作为“卡死绕过”。\n\
   - 风险：客户端可能按 PNG 解码导致异常/崩溃/花屏；也可能触发潜在安全问题。\n\n\
建议默认使用 force-png；遇到 Electron 粘贴卡死时，优先用 force-png 或 spoof-png（临时绕过）。\n\n\
提示：本地化是轻量实现（中/英）；切换语言会立即刷新大部分界面文本。\n"
                .to_string()
        }
        Lang::En => {
            "Image modes (important):\n\n\
1) Force PNG (force-png, recommended)\n\
   - Always convert and provide images as image/png.\n\
   - Best day-to-day compatibility today.\n\n\
2) Multi-MIME (multi)\n\
   - Offer both the original format (e.g. image/jpeg) and a PNG fallback.\n\
   - In theory best-of-both, but known issue: some Electron apps may freeze on paste when relayed JPEG MIME offers exist.\n\
   - If you hit freezes: switch to force-png, or try spoof-png as a workaround.\n\n\
3) Passthrough (passthrough)\n\
   - Keep original MIME (jpeg/webp/gif/png), no conversion.\n\
   - Useful when you know the target apps support those formats.\n\n\
4) Spoof PNG (spoof-png, experimental / risky)\n\
   - Claim image/png but serve original bytes.\n\
   - Can avoid freezes for some Electron+JPG cases.\n\
   - Risk: clients may decode as PNG and crash/garble; also a potential security footgun.\n\n\
Recommendation: use force-png by default. For Electron paste freezes, prefer force-png or (temporarily) spoof-png.\n\n\
Note: localization is intentionally lightweight (ZH/EN). Switching language updates most UI texts immediately.\n"
                .to_string()
        }
    }
}
