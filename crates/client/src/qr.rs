use qrcode::render::svg;
use qrcode::QrCode;

pub fn generate_qr_svg(url: &str) -> Option<String> {
    let code = QrCode::new(url.as_bytes()).ok()?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#00ffcc"))
        .light_color(svg::Color("#111827"))
        .build();
    Some(svg)
}
