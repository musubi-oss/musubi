//! Envelope encode/decode in both wire encodings (plan 6.2).
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use musubi_proto::Encoding;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Input {
    match_id: String,
    op_code: i64,
    tick: u64,
    data: Vec<u8>,
    presences: Vec<String>,
}

/// Code-independent probe of the machine the benches run on (FNV-1a over 64 KiB): CI runners differ
/// in speed from run to run, so `scripts/bench-compare.py` divides every change by this one.
fn calibrate(c: &mut Criterion) {
    let buf: Vec<u8> = (0..65_536u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    c.bench_function("calibrate", |b| {
        b.iter(|| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for &x in black_box(&buf) {
                h = (h ^ x as u64).wrapping_mul(0x0100_0000_01b3);
            }
            black_box(h)
        })
    });
}

fn bench(c: &mut Criterion) {
    let msg = Input {
        match_id: "01a149cf-4f77-7786-b2d6-4f310b496ffc".into(),
        op_code: 7,
        tick: 123_456,
        data: (0..64u8).collect(),
        presences: vec!["01a149cf-4f77-7786-b2d6-4f310b496ffd".into(); 4],
    };
    for enc in [Encoding::Json, Encoding::Postcard] {
        let bytes = enc.encode(&msg).unwrap();
        let mut g = c.benchmark_group(format!("{enc:?}"));
        g.throughput(Throughput::Bytes(bytes.len() as u64));
        g.bench_function("encode", |b| b.iter(|| enc.encode(&msg).unwrap()));
        g.bench_function("decode", |b| {
            b.iter(|| enc.decode::<Input>(&bytes).unwrap())
        });
        g.finish();
    }
}

criterion_group!(benches, calibrate, bench);
criterion_main!(benches);
