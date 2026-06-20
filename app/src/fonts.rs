//! Load Windows system fonts into egui so the Settings / About windows can
//! render every UI language — Latin/Cyrillic/Greek/Arabic (Segoe UI), CJK
//! (YaHei + Yu Gothic + Malgun), Devanagari (Nirmala). Without this, non-Latin
//! scripts show as missing-glyph boxes (egui's default font is Latin-only-ish).
use eframe::egui;

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // (key, path) — appended as fallbacks after egui's default; egui walks the
    // family list until it finds a glyph, so script coverage just stacks.
    let candidates = [
        ("win_segoe", "C:/Windows/Fonts/segoeui.ttf"),   // Latin/Cyrillic/Greek/Arabic
        ("win_yahei", "C:/Windows/Fonts/msyh.ttc"),       // Chinese + CJK ideographs
        ("win_yugo", "C:/Windows/Fonts/YuGothR.ttc"),     // Japanese kana
        ("win_malgun", "C:/Windows/Fonts/malgun.ttf"),    // Korean hangul
        ("win_nirmala", "C:/Windows/Fonts/Nirmala.ttc"),  // Devanagari (Hindi)
    ];
    for (key, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(key.to_string(), egui::FontData::from_owned(bytes));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(key.to_string());
            }
        }
    }
    ctx.set_fonts(fonts);
}
