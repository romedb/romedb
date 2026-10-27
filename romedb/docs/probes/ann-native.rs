use std::time::Instant;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};
fn snapshot(index: &Index) -> Vec<u8> {
    let mut bytes = vec![0; index.serialized_length()];
    index.save_to_buffer(&mut bytes).unwrap();
    bytes
}
fn main() {
    let n = std::env::var("ANN_POINTS")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(10_000usize);
    let dims = 64usize;
    let mut state = 42u64;
    let mut random = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((state >> 32) as u32 as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
    };
    let points: Vec<Vec<f32>> = (0..n)
        .map(|_| (0..dims).map(|_| random()).collect())
        .collect();
    let queries: Vec<Vec<f32>> = (0..64)
        .map(|_| (0..dims).map(|_| random()).collect())
        .collect();
    for metric in [MetricKind::L2sq, MetricKind::Cos] {
        let options = IndexOptions {
            dimensions: dims,
            metric,
            quantization: ScalarKind::F32,
            connectivity: 32,
            expansion_add: 200,
            expansion_search: 512,
            multi: false,
        };
        let index = Index::new(&options).unwrap();
        index.reserve(n + 1).unwrap();
        let now = Instant::now();
        for (key, point) in points.iter().enumerate() {
            index.add(key as u64, point).unwrap();
        }
        let build = now.elapsed();
        let mut recall = 0usize;
        let mut ann_ns = 0;
        let mut exact_ns = 0;
        for i in 0..64 {
            let query = &queries[i];
            let start = Instant::now();
            let ann = index.search(query, 10).unwrap();
            ann_ns += start.elapsed().as_nanos();
            let start = Instant::now();
            let exact = index.exact_search(query, 10).unwrap();
            exact_ns += start.elapsed().as_nanos();
            recall += ann
                .keys
                .iter()
                .filter(|key| exact.keys.contains(key))
                .count();
        }
        assert!(recall >= 608, "recall below 0.95: {recall}/640");
        let start = Instant::now();
        let bytes = snapshot(&index);
        let save = start.elapsed();
        let start = Instant::now();
        let restored = Index::restore_from_buffer(&bytes).unwrap();
        let load = start.elapsed();
        // Expansion is a query policy, explicitly restored by our adapter.
        restored.change_expansion_search(512);
        assert_eq!(restored.size(), n);
        assert_eq!(
            restored.search(&points[1], 10).unwrap().keys,
            index.search(&points[1], 10).unwrap().keys
        );
        assert_eq!(restored.remove(1).unwrap(), 1);
        assert!(!restored.search(&points[1], 10).unwrap().keys.contains(&1));
        restored.reserve(n + 1).unwrap();
        restored.add(1, &points[9999]).unwrap();
        let final_copy = Index::restore_from_buffer(&snapshot(&restored)).unwrap();
        let mut vector = vec![0f32; dims];
        assert_eq!(final_copy.get(1, &mut vector).unwrap(), 1);
        assert_eq!(vector, points[9999]);
        assert_eq!(index.search(&points[1], 1).unwrap().keys, vec![1]);
        println!("metric={:?} n={n} dims={dims} recall={:.4} build_ms={} bytes={} save_us={} load_us={} ann_avg_us={} exact_avg_us={} memory={}", options.metric,recall as f64/640.0,build.as_millis(),bytes.len(),save.as_micros(),load.as_micros(),ann_ns/64000,exact_ns/64000,index.memory_usage());
    }
}
