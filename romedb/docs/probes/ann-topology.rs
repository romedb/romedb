use usearch::{Index, IndexOptions, MetricKind, ScalarKind};
fn main() {
    for upfront in [false, true] {
        let g = Index::new(&IndexOptions {
            dimensions: 2,
            metric: MetricKind::L2sq,
            quantization: ScalarKind::F32,
            connectivity: 32,
            expansion_add: 200,
            expansion_search: 512,
            multi: false,
        })
        .unwrap();
        if upfront {
            g.reserve_capacity_and_threads(256, 1).unwrap();
        }
        for n in 0..256 {
            if !upfront {
                g.reserve(g.size() + 1).unwrap();
            }
            g.add(n, &[n as f32, 1.]).unwrap();
        }
        println!(
            "upfront={upfront} nodes={} upper_layer_nodes={}",
            g.stats_for_level(0).nodes,
            g.stats_for_level(1).nodes
        );
    }
}
