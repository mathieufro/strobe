use clap::{Parser, ValueEnum};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, ValueEnum)]
enum Mode {
    Hot,
    Threads,
    DeepStructs,
    All,
}

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "all")]
    mode: Mode,

    #[arg(long, default_value = "5")]
    duration: u64,
}

// ============ Hot Function Test ============

fn hot_function(counter: &AtomicU64) -> u64 {
    let val = counter.fetch_add(1, Ordering::Relaxed);
    std::hint::black_box(val)
}

fn run_hot_mode(duration_secs: u64) {
    println!("[HOT MODE] Calling hot_function as fast as possible for {} seconds", duration_secs);

    let counter = AtomicU64::new(0);
    let start = Instant::now();

    while start.elapsed().as_secs() < duration_secs {
        hot_function(&counter);
    }

    let total_calls = counter.load(Ordering::Relaxed);
    let elapsed = start.elapsed().as_secs_f64();
    let rate = total_calls as f64 / elapsed;

    println!("[HOT MODE] Called hot_function {} times ({:.0} calls/sec)", total_calls, rate);
    println!("HOT_FUNCTION_RATE={}", rate as u64);
}

// ============ Multi-Threading Test ============

fn worker_function(id: u64, counter: &AtomicU64) -> u64 {
    let val = counter.fetch_add(id, Ordering::Relaxed);
    std::hint::black_box(val)
}

fn run_threads_mode(duration_secs: u64) {
    println!("[THREADS MODE] Spawning 10 worker threads for {} seconds", duration_secs);

    let counter = Arc::new(AtomicU64::new(0));
    let mut handles = vec![];
    let start = Arc::new(Instant::now());

    for i in 0..10 {
        let counter = Arc::clone(&counter);
        let start = Arc::clone(&start);

        let handle = thread::Builder::new()
            .name(format!("worker-{}", i))
            .spawn(move || {
                let mut local_calls = 0u64;

                while start.elapsed().as_secs() < duration_secs {
                    worker_function(i, &counter);
                    local_calls += 1;

                    // Vary call rates:
                    // workers 0-2: fast (no sleep)
                    // workers 3-6: medium (100us sleep)
                    // workers 7-9: slow (10ms sleep)
                    if i >= 7 {
                        thread::sleep(Duration::from_millis(10));
                    } else if i >= 3 {
                        thread::sleep(Duration::from_micros(100));
                    }
                }

                local_calls
            })
            .unwrap();

        handles.push(handle);
    }

    let mut total_calls = 0u64;
    for (i, handle) in handles.into_iter().enumerate() {
        let calls = handle.join().unwrap();
        println!("[THREADS MODE] worker-{}: {} calls", i, calls);
        total_calls += calls;
    }

    let final_counter = counter.load(Ordering::Relaxed);
    println!("[THREADS MODE] Total calls: {}, Counter: {}", total_calls, final_counter);
}

// ============ Deep Struct Test ============

#[repr(C)]
#[derive(Debug)]
struct Level3 {
    value: i32,
    name: [u8; 16],
    data: [u64; 4],
}

#[repr(C)]
#[derive(Debug)]
struct Level2 {
    id: u32,
    timestamp: u64,
    level3: Box<Level3>,
}

#[repr(C)]
#[derive(Debug)]
struct Level1 {
    counter: u64,
    flags: u32,
    level2: Box<Level2>,
}

fn create_deep_struct(counter: u64) -> Level1 {
    Level1 {
        counter,
        flags: 0xDEADBEEF,
        level2: Box::new(Level2 {
            id: (counter % 1000) as u32,
            timestamp: counter * 1000,
            level3: Box::new(Level3 {
                value: (counter % 256) as i32,
                name: *b"deep_struct_test",
                data: [counter, counter * 2, counter * 3, counter * 4],
            }),
        }),
    }
}

fn process_deep_struct(s: &Level1) -> u64 {
    s.counter + s.level2.timestamp + s.level2.level3.value as u64
}

fn run_deep_structs_mode(duration_secs: u64) {
    println!("[DEEP STRUCTS MODE] Creating and processing deep structs for {} seconds", duration_secs);

    let start = Instant::now();
    let mut count = 0u64;
    let mut sum = 0u64;

    while start.elapsed().as_secs() < duration_secs {
        let s = create_deep_struct(count);
        sum += process_deep_struct(&s);
        count += 1;
    }

    println!("[DEEP STRUCTS MODE] Processed {} structs, checksum: {}", count, sum);
}

// ============ Main ============

fn main() {
    let args = Args::parse();

    match args.mode {
        Mode::Hot => run_hot_mode(args.duration),
        Mode::Threads => run_threads_mode(args.duration),
        Mode::DeepStructs => run_deep_structs_mode(args.duration),
        Mode::All => {
            run_hot_mode(args.duration);
            println!();
            run_threads_mode(args.duration);
            println!();
            run_deep_structs_mode(args.duration);
        }
    }
}
