//! Run the candidate filter over a `id<TAB>title<TAB>artist` list on stdin and
//! print the verdict for each row, so the filter can be checked against a real
//! search pool rather than against invented titles.
//!
//! Run: cargo run -p reader --example filter_preview < pool.tsv

use std::io::BufRead;

fn main() {
    let (mut kept, mut dropped) = (0usize, 0usize);
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let mut cols = line.split('\t');
        let (Some(_id), Some(title)) = (cols.next(), cols.next()) else {
            continue;
        };
        let artist = cols.next().unwrap_or("");
        match reader::candidates::reject(title) {
            Some(why) => {
                dropped += 1;
                println!("DROP [{:>13}]  {title}  — {artist}", why.as_str());
            }
            None => {
                kept += 1;
                println!("keep                  {title}  — {artist}");
            }
        }
    }
    eprintln!("\n{kept} kept, {dropped} dropped");
}
