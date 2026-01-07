#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageMode {
    Passthrough,
    ForcePng,
    MultiMime,
    SpoofPng,
}

pub fn parse_image_mode(s: &str) -> anyhow::Result<ImageMode> {
    match s {
        "passthrough" => Ok(ImageMode::Passthrough),
        "force-png" => Ok(ImageMode::ForcePng),
        "multi" | "multi-mime" => Ok(ImageMode::MultiMime),
        "spoof-png" | "fake-png" => Ok(ImageMode::SpoofPng),
        other => anyhow::bail!(
            "invalid --image-mode {}, expected force-png|multi|passthrough|spoof-png",
            other
        ),
    }
}

pub fn image_mode_as_cli_arg(m: ImageMode) -> &'static str {
    match m {
        ImageMode::Passthrough => "passthrough",
        ImageMode::ForcePng => "force-png",
        ImageMode::MultiMime => "multi",
        ImageMode::SpoofPng => "spoof-png",
    }
}
