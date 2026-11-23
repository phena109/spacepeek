//! One-off raster compare. Deleted after the icon edge is baked.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let svg_path = args.first().map(String::as_str).unwrap_or("assets/spacepeek.svg");
    let out_dir = args.get(1).map(String::as_str).unwrap_or("/tmp/rend");
    let _ = std::fs::create_dir_all(out_dir);
    let svg = std::fs::read_to_string(svg_path).expect("svg");
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(&svg, &opt).expect("parse");
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 256];
    for size in sizes {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("pixmap");
        let scale = size as f32 / tree.size().width();
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        let mut raw = Vec::with_capacity((size * size * 4) as usize);
        for px in pixmap.pixels() {
            let c = px.demultiply();
            raw.push(c.red());
            raw.push(c.green());
            raw.push(c.blue());
            raw.push(c.alpha());
        }
        let name = format!("{out_dir}/{size}.rgba");
        std::fs::write(&name, raw).expect("write");
        println!("wrote {name}");
    }
}
