// This demonstrates the issue: std::fs::File::lock() uses flock on Unix which is 
// a process-level advisory lock. Multiple threads in the SAME process can acquire
// the same flock lock - they don't block each other!

use std::sync::{Arc, Barrier};
use std::thread;
use std::fs::OpenOptions;

fn main() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("source-key.ed25519");
    let lock_path = path.with_file_name(format!("{}.lock", path.file_name().unwrap().to_string_lossy()));
    
    const WORKERS: usize = 12;
    let barrier = Arc::new(Barrier::new(WORKERS));
    let keys = thread::scope(|scope| {
        let tasks: Vec<_> = (0..WORKERS)
            .map(|_| {
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
                    
                    // Simulate key generation
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let key = vec![1,2,3,4]; // dummy key
                    
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
}
