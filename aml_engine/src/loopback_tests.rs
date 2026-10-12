use crate::loopback::{jitter_ms, mean_ms, percentile_nearest_rank_ms};

#[test]
fn latency_mean_and_jitter() {
    let v = vec![100.0, 110.0, 90.0, 100.0];
    assert!((mean_ms(&v) - 100.0).abs() < 1e-9);
    assert!((jitter_ms(&v) - 7.071067811865475).abs() < 1e-6);
    assert_eq!(jitter_ms(&[]), 0.0);
    assert_eq!(jitter_ms(&[42.0]), 0.0);
}

#[test]
fn latency_p95_nearest_rank() {
    let v: Vec<f64> = (1..=20).map(|i| i as f64).collect();
    assert_eq!(percentile_nearest_rank_ms(v.clone(), 95.0), 19.0);
    assert_eq!(percentile_nearest_rank_ms(v, 50.0), 10.0);
}

/// Idle time must not become a credit that dumps a later packet in one pull.
#[test]
fn paced_source_plays_a_burst_at_sample_rate() {
    use crate::audio::{AudioSink, AudioSource};
    use crate::testing::MemoryAudio;
    use std::thread;
    use std::time::{Duration, Instant};

    let (mut sink, source) = MemoryAudio::pair(48_000, 100_000);
    let mut paced = source.into_paced();
    paced.start().unwrap();
    let mut buf = [0.0f32; 512];
    for _ in 0..5 {
        assert_eq!(paced.pull(&mut buf), 0);
        thread::sleep(Duration::from_millis(30));
    }
    // Last empty pull clears credit accumulated during the sleeps above.
    assert_eq!(paced.pull(&mut buf), 0);

    let n = 48_000 / 10; // 100 ms of audio
    assert_eq!(sink.push(&vec![0.2; n]), n);
    let t0 = Instant::now();
    let mut got = 0usize;
    while got < n {
        let k = paced.pull(&mut buf);
        got += k;
        if k == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    let elapsed = t0.elapsed();
    assert!(
        elapsed >= Duration::from_millis(80),
        "burst released too fast: {:?}",
        elapsed
    );
    assert!(
        elapsed < Duration::from_millis(250),
        "burst released too slow: {:?}",
        elapsed
    );
}
