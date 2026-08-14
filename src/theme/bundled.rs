//! The bundled theme set: `themes/*.toml` embedded at compile time. Fifteen
//! well-known palettes (design contract): catppuccin ×4, kanagawa, tokyonight
//! ×3, gruvbox, dracula, solarized, nord, rose-pine, one-dark, everforest.

/// (name, embedded TOML) pairs; names match each file's `name` field.
pub const BUNDLED: &[(&str, &str)] = &[
    (
        "catppuccin-latte",
        include_str!("../../themes/catppuccin-latte.toml"),
    ),
    (
        "catppuccin-frappe",
        include_str!("../../themes/catppuccin-frappe.toml"),
    ),
    (
        "catppuccin-macchiato",
        include_str!("../../themes/catppuccin-macchiato.toml"),
    ),
    (
        "catppuccin-mocha",
        include_str!("../../themes/catppuccin-mocha.toml"),
    ),
    (
        "kanagawa-wave",
        include_str!("../../themes/kanagawa-wave.toml"),
    ),
    (
        "tokyonight-night",
        include_str!("../../themes/tokyonight-night.toml"),
    ),
    (
        "tokyonight-storm",
        include_str!("../../themes/tokyonight-storm.toml"),
    ),
    (
        "tokyonight-day",
        include_str!("../../themes/tokyonight-day.toml"),
    ),
    (
        "gruvbox-dark",
        include_str!("../../themes/gruvbox-dark.toml"),
    ),
    ("dracula", include_str!("../../themes/dracula.toml")),
    (
        "solarized-dark",
        include_str!("../../themes/solarized-dark.toml"),
    ),
    ("nord", include_str!("../../themes/nord.toml")),
    ("rose-pine", include_str!("../../themes/rose-pine.toml")),
    ("one-dark", include_str!("../../themes/one-dark.toml")),
    (
        "everforest-dark",
        include_str!("../../themes/everforest-dark.toml"),
    ),
];
