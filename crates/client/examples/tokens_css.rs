//! Régénère `design/tokens.css` depuis `design/tokens.json`.
//! `cargo run -p matter-home-desktop --example tokens_css`

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design");
    let json = std::fs::read_to_string(dir.join("tokens.json")).expect("lecture de tokens.json");
    let css = matter_home_client::tokens_css::generate(&json).expect("conversion");
    std::fs::write(dir.join("tokens.css"), css).expect("écriture de tokens.css");
}
