//! App settings helpers (persistence lives in database::repo; this module
//! centralizes defaults and validation shared by commands).

pub const KEYS: &[&str] = &[
    "theme", "history_enabled", "auto_thumbnail", "subtitle_lang", "audio_lang",
    "default_speed", "default_volume", "keep_awake",
];

pub fn is_allowed_key(k: &str) -> bool {
    KEYS.contains(&k)
}
