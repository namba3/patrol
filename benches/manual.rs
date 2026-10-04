use std::hint::black_box;
use std::time::{Duration, Instant};

use patrol::domain::{Hash, Selector};

const SAMPLE_COUNT: usize = 5;
const HASH_BYTES_PER_SAMPLE: usize = 8 * 1024 * 1024;
const SELECTOR_ITERATIONS: usize = 10_000;

fn main() {
    println!("Patrol manual benchmarks (median of {SAMPLE_COUNT} samples)");

    for size in [64, 4 * 1024, 1024 * 1024] {
        let input = vec![b'x'; size];
        let iterations = (HASH_BYTES_PER_SAMPLE / size).max(10);
        measure(&format!("SHA-256 ({size} B)"), iterations, || {
            Hash::new(black_box(input.as_slice()))
        });
    }

    let selector = "main article section.content h2.title";
    measure("CSS selector parse", SELECTOR_ITERATIONS, || {
        Selector::new(black_box(selector.to_owned())).expect("benchmark selector is valid")
    });
}

fn measure<T>(name: &str, iterations: usize, mut operation: impl FnMut() -> T) {
    let warmup_iterations = (iterations / 10).clamp(1, 1_000);
    for _ in 0..warmup_iterations {
        black_box(operation());
    }

    let mut samples = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(operation());
        }
        samples.push(started.elapsed());
    }

    samples.sort_unstable();
    let median = samples[SAMPLE_COUNT / 2];
    print_result(name, iterations, median);
}

fn print_result(name: &str, iterations: usize, elapsed: Duration) {
    let nanos_per_operation = elapsed.as_nanos() as f64 / iterations as f64;
    println!("{name}: {nanos_per_operation:.1} ns/op ({iterations} iterations/sample)");
}
