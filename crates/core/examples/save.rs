//! Developer tool: save one page into an archive folder from the command line.
//! cargo run -p persistwise-core --example save -- <data-dir> <url> [--wayback]
use persistwise_core::{capture::CaptureOptions, store::Store, wayback::Wayback};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (dir, url) = (args.get(1).expect("data dir"), args.get(2).expect("url"));
    let store = Store::open(dir)?;
    let t = std::time::Instant::now();
    let page = persistwise_core::save_page(&store, url, &CaptureOptions::default())?;
    println!(
        "saved {:?} in {:.1}s: {} bytes, {} inlined, {} not inlined -> {}",
        page.title,
        t.elapsed().as_secs_f32(),
        page.size_bytes.unwrap_or(0),
        page.assets_inlined,
        page.assets_failed,
        store.file_path(&page).unwrap().display()
    );
    if args.iter().any(|a| a == "--wayback") {
        let p = persistwise_core::backup_wayback(&store, page.id, &Wayback::default())?;
        println!(
            "wayback: {} {:?} {:?}",
            p.wayback_status, p.wayback_url, p.wayback_error
        );
    }
    Ok(())
}
