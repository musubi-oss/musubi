//! Envelope encode/decode in both wire encodings (plan 6.2).
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
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

criterion_group!(benches, bench);
criterion_main!(benches);
