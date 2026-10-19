//! Micro-benchmarks for the meshing core (plan 6.2): handover detection over a populated world,
//! the per-tick advance, cell views and client deltas. Compared against the `main` baseline in CI.
use std::collections::BTreeMap;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use musubi_mesh::{Pos, World, WorldConfig};
use uuid::Uuid;

const SIZE: f64 = 256.0;

fn populated(n: usize, cells: i64) -> (World, Uuid, Vec<musubi_mesh::EntityId>) {
    let cfg = WorldConfig::default();
    let size = cfg.cell_size;
    let mut w = World::new(cfg);
    let sim = Uuid::now_v7();
    for x in -cells..=cells {
        for y in -cells..=cells {
            let p = Pos {
                x: x as f64 * size + 1.0,
                y: y as f64 * size + 1.0,
                z: 0.0,
            };
            w.grant_lease(w.cell_of(p), sim);
        }
    }
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / n as f64;
        let p = Pos {
            x: (t * 2.0 - 1.0) * cells as f64 * size * 0.9,
            y: ((i * 7919) % 1000) as f64 / 1000.0 * size * 0.9,
            z: 0.0,
        };
        ids.push(w.spawn(p, BTreeMap::new(), false));
    }
    (w, sim, ids)
}

fn bench(c: &mut Criterion) {
    for &n in &[1_000usize, 10_000] {
        c.bench_function(&format!("detect_handovers/{n}"), |b| {
            b.iter_batched(
                || populated(n, 4),
                |(mut w, sim, ids)| {
                    // push every entity one cell to the right so each crosses a border
                    for id in &ids {
                        let p = w.get(*id).map(|e| e.pos).unwrap();
                        let _ = w.apply(
                            sim,
                            *id,
                            Some(Pos {
                                x: p.x + SIZE,
                                y: p.y,
                                z: 0.0,
                            }),
                            BTreeMap::new(),
                        );
                    }
                    w.detect_handovers()
                },
                BatchSize::LargeInput,
            )
        });
        c.bench_function(&format!("advance/{n}"), |b| {
            let (mut w, _, _) = populated(n, 4);
            b.iter(|| w.advance())
        });
        c.bench_function(&format!("view/{n}"), |b| {
            let (w, _, ids) = populated(n, 4);
            let cell = w.get(ids[n / 2]).unwrap().cell;
            b.iter(|| w.view(cell))
        });
        c.bench_function(&format!("delta_for/{n}"), |b| {
            let (mut w, _, ids) = populated(n, 4);
            let client = Uuid::now_v7();
            let p = w.get(ids[n / 2]).unwrap().pos;
            w.subscribe(client, p, 2.0 * SIZE);
            b.iter(|| w.delta_for(client))
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
