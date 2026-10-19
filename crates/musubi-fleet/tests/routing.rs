//! Latency/geo-aware ordering of a pool's fleets for routed allocation.

use std::collections::HashMap;

use musubi_fleet::{haversine_km, rank_fleets, Fleet, GeoPoint, RouteHint};

fn fleet(name: &str, region: &str) -> Fleet {
    serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::now_v7(), "name": name, "provider": "local", "region": region, "pool": "p",
        "spec": {}, "buffer": 0, "max_servers": 1, "min_servers": 0, "policy": {},
        "port_start": 7000, "port_end": 7010, "created_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap()
}

fn pool() -> Vec<Fleet> {
    vec![
        fleet("eu", "eu-west"),
        fleet("us", "us-east"),
        fleet("ap", "ap-south"),
    ]
}

fn coords() -> HashMap<String, GeoPoint> {
    [
        (
            "eu-west",
            GeoPoint {
                lat: 53.3,
                lon: -6.3,
            },
        ),
        (
            "us-east",
            GeoPoint {
                lat: 39.0,
                lon: -77.5,
            },
        ),
        (
            "ap-south",
            GeoPoint {
                lat: 19.1,
                lon: 72.9,
            },
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn order(r: &[(Fleet, serde_json::Value)]) -> Vec<(&str, &str)> {
    r.iter()
        .map(|(f, w)| (f.region.as_str(), w["reason"].as_str().unwrap()))
        .collect()
}

#[test]
fn pings_rank_lowest_first_and_ceiling_drops_regions() {
    let hint = RouteHint {
        pings: [("eu-west", 120.0), ("us-east", 30.0), ("ap-south", 250.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        ..Default::default()
    };
    let r = rank_fleets(pool(), &hint, &HashMap::new());
    assert_eq!(
        order(&r),
        vec![
            ("us-east", "ping"),
            ("eu-west", "ping"),
            ("ap-south", "ping")
        ]
    );
    assert_eq!(r[0].1["ping_ms"], 30.0);
    assert_eq!(r[0].1["rank"], 0);
    let strict = RouteHint {
        max_ping_ms: Some(150.0),
        ..hint.clone()
    };
    let r = rank_fleets(pool(), &strict, &HashMap::new());
    assert_eq!(order(&r), vec![("us-east", "ping"), ("eu-west", "ping")]);
    let none = RouteHint {
        max_ping_ms: Some(10.0),
        ..hint
    };
    assert!(rank_fleets(pool(), &none, &HashMap::new()).is_empty());
}

#[test]
fn explicit_regions_beat_pings_then_distance_then_hint() {
    let hint = RouteHint {
        regions: vec!["ap-south".into()],
        pings: [("us-east".to_string(), 30.0)].into_iter().collect(),
        client: Some(GeoPoint {
            lat: 48.8,
            lon: 2.3,
        }),
        region: Some("eu-west".into()),
        ..Default::default()
    };
    let r = rank_fleets(pool(), &hint, &coords());
    // ap: preferred; us: ping; eu: distance (coords known) — the plain hint only matters without coords
    assert_eq!(
        order(&r),
        vec![
            ("ap-south", "preferred"),
            ("us-east", "ping"),
            ("eu-west", "distance")
        ]
    );
    assert!(r[2].1["distance_km"].as_f64().unwrap() < 1000.0);
    let hint_only = RouteHint::region(Some("us-east"));
    let r = rank_fleets(pool(), &hint_only, &HashMap::new());
    assert_eq!(
        order(&r),
        vec![
            ("us-east", "hint"),
            ("ap-south", "fallback"),
            ("eu-west", "fallback")
        ]
    );
}

#[test]
fn nearest_region_by_coordinates() {
    let hint = RouteHint {
        client: Some(GeoPoint {
            lat: 40.7,
            lon: -74.0,
        }),
        ..Default::default()
    };
    let r = rank_fleets(pool(), &hint, &coords());
    assert_eq!(
        order(&r),
        vec![
            ("us-east", "distance"),
            ("eu-west", "distance"),
            ("ap-south", "distance")
        ]
    );
    let d = haversine_km(
        GeoPoint { lat: 0.0, lon: 0.0 },
        GeoPoint { lat: 0.0, lon: 1.0 },
    );
    assert!((d - 111.2).abs() < 1.0, "{d}");
    assert!(RouteHint {
        client: Some(GeoPoint {
            lat: 91.0,
            lon: 0.0
        }),
        ..Default::default()
    }
    .validate()
    .is_err());
    assert!(RouteHint {
        max_ping_ms: Some(0.0),
        ..Default::default()
    }
    .validate()
    .is_err());
}
