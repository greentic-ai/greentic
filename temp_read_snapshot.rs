fn main() {
    let bytes = std::fs::read("greentic/imports/snapshot-20251003-104153.gtc").unwrap();
    println!("size {}", bytes.len());
    for b in bytes.iter().take(10) {
        print!("{:02x} ", b);
    }
    println!();
}
