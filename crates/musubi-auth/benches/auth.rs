//! Password hashing/verification cost (plan 6.2) — the argon2 parameters are the login hot path.
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use musubi_auth::password;

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let hash = rt
        .block_on(password::hash("correct horse battery staple".into()))
        .unwrap();
    c.bench_function("argon2/hash", |b| {
        b.iter(|| {
            rt.block_on(password::hash("correct horse battery staple".into()))
                .unwrap()
        })
    });
    c.bench_function("argon2/verify", |b| {
        b.iter(|| {
            rt.block_on(password::verify(
                "correct horse battery staple".into(),
                hash.clone(),
            ))
            .unwrap()
        })
    });
    c.bench_function("password/validate", |b| {
        b.iter(|| black_box(password::validate(black_box("Tr0ub4dor&3xyz"))))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
