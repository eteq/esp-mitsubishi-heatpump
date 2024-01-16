#![allow(dead_code)]

pub fn checksum(rvec: Vec<u8>) -> u8 {
    let mut sum = 0u8;
    for b in rvec.iter() {
        sum += b;
    }
    0xfc - sum
}