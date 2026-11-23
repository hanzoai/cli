// This demonstrates the issue: std::fs::File::lock() uses flock on Unix which is 
// a process-level advisory lock. Multiple threads in the SAME process can acquire
// the same flock lock - they don't block each other!

use std::sync::{Arc, Barrier};
use std::thread;
use std::fs::OpenOptions;

#[test]
fn flock_does_not_serialize_threads_in_same_process() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("source-key.ed25519");
    let lock_path = path.with_file_name(format!("{}.lock", path.file_name().unwrap().to_string_lossy()));
    
    const WORKERS: usize = 12;
    let barrier = Arc::new(Barrier::new(WORKERS));
    let keys = thread::scope(|scope| {
        let tasks: Vec<_> = (0..WORKERS)
            .map(|i| {
                let lock_path = lock_path.clone();
                let start = barrier.clone();
                scope.spawn(move || {
                    start.wait();
                    let f = OpenOptions::new()
                        .create(true)
                        .read(true)
                        .write(true)
                        .truncate(false)
                        .open(&lock_path)
                        .unwrap();
                    
                    // This uses flock on Unix - PROCESS-LEVEL lock
                    // Multiple threads in the SAME process can ALL acquire it!
                    f.lock().unwrap();
                    
                    // Simulate key generation - use thread ID to make unique keys
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let key = format!("key-from-thread-{}", i).into_bytes();
                    
                    f.unlock().unwrap();
                    key
                })
            })
            .collect();
        tasks.into_iter().map(|task| task.join().unwrap()).collect::<Vec<_>>()
    });
    println!("Keys generated: {}", keys.len());
    let all_same = keys.iter().all(|key| key == &keys[0]);
    println!("All keys same: {}", all_same);
    println!("If true -> flock DIDN'T serialize threads (same process)!");
    println!("If false -> flock DID serialize (unexpected!)");
    
    // With flock, threads in the SAME process don't block each other
    // So all 12 threads will run concurrently and generate different keys
    assert!(!all_same, "flock should NOT serialize threads in same process!");
}
