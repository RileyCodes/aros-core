//! Network utilities — ping monitor, connectivity checks

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

const PING_WINDOW: usize = 30;
const PING_INTERVAL_MS: u64 = 1000;
const PING_TIMEOUT_MS: u64 = 2000;

#[derive(Debug, Clone)]
pub struct PingStats {
    pub avg_ms: u64,
    pub jitter_ms: u64,
    pub loss_pct: u8,
}

impl PingStats {
    pub fn format(&self) -> String {
        if self.loss_pct >= 100 {
            "offline".to_string()
        } else {
            format!("{}ms ±{} {}%↓", self.avg_ms, self.jitter_ms, self.loss_pct)
        }
    }
}

#[derive(Clone)]
pub struct PingMonitor {
    results: Arc<Mutex<Vec<i64>>>, // -1 = lost, else ms
}

impl PingMonitor {
    pub fn new() -> Self {
        Self {
            results: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Run the ping loop, calling the callback with formatted status
    pub async fn run<F>(&self, callback: F)
    where
        F: Fn(&str) + Send + 'static,
    {
        loop {
            let result = tcp_ping("8.8.8.8:443", PING_TIMEOUT_MS).await;

            {
                let mut results = self.results.lock().await;
                results.push(result);
                while results.len() > PING_WINDOW {
                    results.remove(0);
                }
            }

            let stats = self.stats().await;
            callback(&stats.format());

            tokio::time::sleep(Duration::from_millis(PING_INTERVAL_MS)).await;
        }
    }

    pub async fn stats(&self) -> PingStats {
        let results = self.results.lock().await;
        compute_stats(&results)
    }
}

impl Default for PingMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// TCP connect ping to measure latency
async fn tcp_ping(addr: &str, timeout_ms: u64) -> i64 {
    let start = Instant::now();
    match tokio::time::timeout(Duration::from_millis(timeout_ms), TcpStream::connect(addr)).await {
        Ok(Ok(_)) => start.elapsed().as_millis() as i64,
        _ => -1,
    }
}

fn compute_stats(results: &[i64]) -> PingStats {
    if results.is_empty() {
        return PingStats {
            avg_ms: 0,
            jitter_ms: 0,
            loss_pct: 100,
        };
    }

    let lost = results.iter().filter(|&&r| r == -1).count();
    let successful: Vec<f64> = results
        .iter()
        .filter(|&&r| r >= 0)
        .map(|&r| r as f64)
        .collect();
    let loss_pct = ((lost * 100) / results.len()) as u8;

    if successful.is_empty() {
        return PingStats {
            avg_ms: 0,
            jitter_ms: 0,
            loss_pct: 100,
        };
    }

    let avg = successful.iter().sum::<f64>() / successful.len() as f64;
    let jitter = if successful.len() > 1 {
        let variance =
            successful.iter().map(|&x| (x - avg).powi(2)).sum::<f64>() / successful.len() as f64;
        variance.sqrt() as u64
    } else {
        0
    };

    PingStats {
        avg_ms: avg as u64,
        jitter_ms: jitter,
        loss_pct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_stats_all_success() {
        let results = vec![50, 60, 55, 45, 50];
        let stats = compute_stats(&results);
        assert_eq!(stats.avg_ms, 52);
        assert_eq!(stats.loss_pct, 0);
        assert!(stats.jitter_ms < 10);
    }

    #[test]
    fn test_compute_stats_some_loss() {
        let results = vec![50, -1, 60, -1, 55];
        let stats = compute_stats(&results);
        assert_eq!(stats.loss_pct, 40);
        assert_eq!(stats.avg_ms, 55); // (50+60+55)/3 = 55
    }

    #[test]
    fn test_compute_stats_all_lost() {
        let results = vec![-1, -1, -1];
        let stats = compute_stats(&results);
        assert_eq!(stats.loss_pct, 100);
        assert_eq!(stats.format(), "offline");
    }

    #[test]
    fn test_compute_stats_empty() {
        let results: Vec<i64> = vec![];
        let stats = compute_stats(&results);
        assert_eq!(stats.loss_pct, 100);
    }

    #[test]
    fn test_format() {
        let stats = PingStats {
            avg_ms: 42,
            jitter_ms: 5,
            loss_pct: 3,
        };
        assert_eq!(stats.format(), "42ms ±5 3%↓");
    }

    #[test]
    fn test_compute_stats_single() {
        let results = vec![100];
        let stats = compute_stats(&results);
        assert_eq!(stats.avg_ms, 100);
        assert_eq!(stats.jitter_ms, 0);
        assert_eq!(stats.loss_pct, 0);
    }
}
