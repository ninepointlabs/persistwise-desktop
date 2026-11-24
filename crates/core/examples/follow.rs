//! Developer tool: follow a feed in an archive folder (does not check it).
//! cargo run -p persistwise-core --example follow -- <data-dir> <feed-or-site-url>
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let store = persistwise_core::store::Store::open(&args[1])?;
    let (feed, created) = persistwise_core::feeds::subscribe(&store, &args[2])?;
    println!(
        "{} feed {} -> {}",
        if created {
            "following"
        } else {
            "already following"
        },
        feed.name.unwrap_or_default(),
        feed.url
    );
    Ok(())
}
