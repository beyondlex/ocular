use std::time::Duration;
use tokio::sync::broadcast;

/// Integration test: requires Redis on 127.0.0.1:6379 and sudo/BPF permissions.
/// Run with: sudo cargo test -p ocular-capture --test integration -- --nocapture
#[tokio::test]
#[ignore] // requires sudo/BPF permissions and Redis
async fn capture_redis_traffic() {
    let (tx, mut rx) = broadcast::channel::<ocular_protocol::ProxyEvent>(1024);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let config = ocular_capture::CaptureConfig {
        name: "redis-test".to_string(),
        protocol: ocular_protocol::Protocol::Redis,
        interface: "lo0".to_string(),
        remote: "127.0.0.1:6379".to_string(),
    };

    let tx_clone = tx.clone();
    tokio::spawn(async move {
        if let Err(e) = ocular_capture::run_capture(config, tx_clone, shutdown_rx).await {
            eprintln!("capture error: {}", e);
        }
    });

    // Give capture time to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send redis commands
    let output = std::process::Command::new("redis-cli")
        .args(["SET", "ocular_capture_test", "hello"])
        .output()
        .expect("redis-cli not found");
    assert!(output.status.success(), "redis-cli SET failed");

    let output = std::process::Command::new("redis-cli")
        .args(["GET", "ocular_capture_test"])
        .output()
        .expect("redis-cli not found");
    assert!(output.status.success(), "redis-cli GET failed");

    // Wait for events to be captured and processed
    tokio::time::sleep(Duration::from_secs(1)).await;

    let _ = shutdown_tx.send(true);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut events = vec![];
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    println!("Captured {} events:", events.len());
    for ev in &events {
        println!(
            "  [{}] {} -> {} ({:.2}ms)",
            ev.component,
            ev.command,
            ev.response,
            ev.latency.as_secs_f64() * 1000.0
        );
    }

    assert!(!events.is_empty(), "Expected at least one captured event");
    assert!(
        events.iter().any(|e| e.command.contains("SET")),
        "Expected a SET command in captured events"
    );
}
