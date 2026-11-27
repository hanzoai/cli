use super::*;
use hanzo_gen::context::Gpu;
use ratatui::{backend::TestBackend, Terminal};

fn spark_machine() -> Machine {
    Machine {
        spec: Spec {
            os: "linux".into(),
            arch: "aarch64".into(),
            cpus: 20,
            memory: 124610 * 1048576,
            gpus: vec![Gpu {
                vendor: "nvidia".into(),
                model: "GB10".into(),
                memory: 0,
            }],
            ..Default::default()
        },
        metrics: Metrics {
            load1: 3.2,
            mem_used: 92096 * 1048576,
            mem_free: 32514 * 1048576,
            gpu_util: 0.15,
            cpu_util: Some(0.56),
            cpu_temp: Some(80.3),
            gpu_temp: Some(54.0),
            gpu_power: Some(16.5),
            gpu_mem_used: Some(73085 * 1048576),
            disk_used: Some(3033975 * 1048576),
            disk_total: Some(3649741 * 1048576),
            disk_read: Some(149_755_753.0),
            disk_write: Some(70_211_646.0),
            net_rx: Some(773_675.0),
            net_tx: Some(143_806.0),
            model: Some("qwen3.8-flash-next".into()),
            decode: Some(41.2),
            prefill: Some(1200.0),
            ttft: Some(0.21),
            running: Some(1),
            waiting: Some(0),
            kv_cache: Some(0.12),
            ..Default::default()
        },
    }
}

fn target(id: &str, host: &str, status: &str, at: &str) -> Target {
    let mut t: Target = serde_json::from_value(serde_json::json!({
        "id": id, "label": host, "kind": "gpu", "status": status, "host": host,
        "spec": {"os": "linux", "arch": "amd64", "cpus": 32, "memory": 68719476736i64,
                 "gpus": [{"vendor": "nvidia", "model": "RTX 4090", "memory": 25757220864i64}]},
        "metrics": {"load1": 1.0, "memUsed": 34359738368i64, "gpuUtil": 0.5, "gpuTemp": 61.0,
                    "gpuMemUsed": 12884901888i64, "gpuMemTotal": 25757220864i64, "at": 1},
        "metricsAt": at, "sessions": 0, "running": 0
    }))
    .unwrap();
    t.label = host.into();
    t
}

#[test]
fn this_machine_leads_and_replaces_its_own_target() {
    let mut b = Board {
        host: "dgx".into(),
        ..Default::default()
    };
    b.apply(Update::Local(Box::new(spark_machine())));
    b.apply(Update::Fleet(Ok(vec![
        target("tgt_old", "evo", "offline", "2026-09-08T04:51:59Z"),
        target("tgt_older", "evo", "offline", "2026-08-01T00:00:00Z"),
        target("tgt_me", "dgx", "online", "2026-09-23T07:00:00Z"),
        target("tgt_live", "rig", "online", "2026-09-23T07:00:00Z"),
    ])));
    let units = b.units();
    let names: Vec<&str> = units.iter().map(|u| u.label.as_str()).collect();
    assert_eq!(
        names,
        ["dgx", "rig", "evo"],
        "local first, online before offline, own target folded in"
    );
    assert!(units[0].local && units[0].metrics.model.is_some());
    assert!(!units[2].online);
    assert_eq!(
        units[2].key, "tgt_old",
        "a re-registered host shows its newest target"
    );
}

#[test]
fn history_takes_a_sibling_reading_once() {
    let mut b = Board::default();
    let t = target("tgt_1", "rig", "online", "2026-09-23T07:00:00Z");
    b.apply(Update::Fleet(Ok(vec![t.clone()])));
    b.apply(Update::Fleet(Ok(vec![t.clone()])));
    assert_eq!(
        b.history["tgt_1"].gpu.len(),
        1,
        "the same reading is not a new point"
    );
    let mut later = t;
    later.metrics_at = Some("2026-09-23T07:00:30Z".into());
    b.apply(Update::Fleet(Ok(vec![later])));
    assert_eq!(b.history["tgt_1"].gpu.len(), 2);
    assert_eq!(
        b.history["tgt_1"].cpu_temp.len(),
        0,
        "an absent reading is no point, not a zero"
    );
}

#[test]
fn memory_mode_follows_dedicated_vram() {
    let m = spark_machine();
    assert!(unified(&m.spec, &m.metrics));
    let t = target("t", "rig", "online", "2026-09-23T07:00:00Z");
    assert!(!unified(&t.spec, &t.metrics));
    assert!(
        !unified(&Spec::default(), &Metrics::default()),
        "no GPU is not unified memory"
    );
}

#[test]
fn values_read_like_sparkdash() {
    assert_eq!(gib(92096 * 1048576), "89.9 GiB");
    assert_eq!(gib(3649741 * 1048576), "3.5 TiB");
    assert_eq!(rate(773_675.0), "755.5 KiB/s");
    assert_eq!(rate(12.0), "12 B/s");
    assert_eq!(tokens(41.23), "41.2");
    assert_eq!(tokens(1234.0), "1.2k");
    assert_eq!(seconds(0.21), "210 ms");
    assert_eq!(seconds(2.5), "2.50 s");
    assert_eq!(age(30), "now");
    assert_eq!(age(15 * 86_400), "15d");
    let q: VecDeque<f64> = [0.0, 50.0, 100.0].into_iter().collect();
    assert_eq!(spark(&q, 5, Some(100.0)), "  ▁▅█");
    assert_eq!(
        spark(&VecDeque::from([7.0]), 3, None),
        "   ",
        "one point is no line"
    );
    assert_eq!(bar(0.5, 4), "██░░");
    assert_eq!(
        (gpu_temp_color(70.0), cpu_temp_color(70.0)),
        (Color::Yellow, ACCENT)
    );
    assert_eq!(load_color(90.0), Color::Red);
    assert_eq!(
        product("Advanced Micro Devices, Inc. [AMD/ATI] Strix [Radeon 8060S]"),
        "Radeon 8060S"
    );
    assert_eq!(
        product("Advanced Micro Devices, Inc. [AMD/ATI] Device 1586"),
        "AMD/ATI Device 1586"
    );
    assert_eq!(product("GB10"), "GB10");
}

#[test]
fn renders_the_home_page() {
    let mut b = Board {
        host: "dgx".into(),
        ..Default::default()
    };
    for _ in 0..3 {
        b.apply(Update::Local(Box::new(spark_machine())));
    }
    b.apply(Update::Fleet(Ok(vec![target(
        "tgt_old",
        "evo",
        "offline",
        "2026-09-08T04:51:59Z",
    )])));
    b.apply(Update::Networks(Ok(vec![])));
    b.apply(Update::Accounts {
        active: Some("hanzo/z".into()),
        all: vec!["hanzo/z".into()],
        api: "https://api.hanzo.ai".into(),
    });
    let mut term = Terminal::new(TestBackend::new(136, 32)).unwrap();
    term.draw(|f| render(f, f.area(), &b)).unwrap();
    let text: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    for want in [
        "dgx · GB10",
        "this machine",
        "unified",
        "gpu 71.4 · cpu",
        "qwen3.8-flash-next",
        "41.2 tok/s",
        "210 ms",
        "kv 12.0%",
        "evo",
        "RTX 4090",
        "machines · hanzo/z",
        "zero trust",
        "hanzo/z",
        "GPU util",
        "decode  41.2 tok/s",
    ] {
        assert!(text.contains(want), "missing {want:?}");
    }
}
