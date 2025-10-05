use std::{fs::File, io::BufReader};
use serde_cbor::Value;

fn main() {
    let file = File::open("greentic/imports/snapshot-20251005-072754.gtc").unwrap();
    let reader = BufReader::new(file);
    let value: Value = serde_cbor::from_reader(reader).unwrap();
    println!("{:?}", value);
}
