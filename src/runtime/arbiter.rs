//! Sharing GPUs between models. Each GPU has a memory budget; a model that needs room gets it
//! by stopping the least recently used idle models on that GPU, and waits while the ones in the
//! way are busy. A model in use is never stopped under a running request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Notify;

#[derive(Debug, Clone, Serialize)]
pub struct Occupant {
    pub key: String,
    pub vram_mb: u64,
    pub active: usize,
    #[serde(skip)]
    pub last_used: Instant,
}

#[derive(Default)]
pub struct Arbiter {
    gpus: Mutex<HashMap<String, Vec<Occupant>>>,
    changed: Notify,
}

/// Holding a lease keeps its model on the GPU; dropping it marks the model idle.
pub struct Lease {
    arbiter: Arc<Arbiter>,
    gpu: String,
    key: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut gpus = self.arbiter.gpus.lock().unwrap();
        if let Some(o) = gpus.get_mut(&self.gpu).and_then(|v| v.iter_mut().find(|o| o.key == self.key)) {
            o.active = o.active.saturating_sub(1);
            o.last_used = Instant::now();
        }
        drop(gpus);
        self.arbiter.changed.notify_waiters();
    }
}

/// What `acquire` decided while holding the lock.
enum Step {
    Granted,
    /// Stop these (already removed from the GPU's list), then try again.
    Evict(Vec<String>),
    /// Something in the way is busy: wait for a change.
    Wait,
}

impl Arbiter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Room on `gpu` for `key` (needing `need_mb` of `budget_mb`). Occupants whose key starts
    /// with `exclusive` (other than `key`) must go regardless of room (e.g. the other image or
    /// video model of the same server). `stop` unloads an evicted model.
    pub async fn acquire<F, Fut>(
        self: &Arc<Self>,
        gpu: &str,
        budget_mb: u64,
        key: &str,
        need_mb: u64,
        exclusive: Option<&str>,
        stop: F,
    ) -> Lease
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let step = self.decide(gpu, budget_mb, key, need_mb, exclusive);
            match step {
                Step::Granted => return Lease { arbiter: self.clone(), gpu: gpu.to_string(), key: key.to_string() },
                Step::Evict(keys) => {
                    for k in keys {
                        tracing::info!("freeing {gpu} for {key}: stopping {k}");
                        stop(k).await;
                    }
                    self.changed.notify_waiters();
                }
                Step::Wait => notified.await,
            }
        }
    }

    fn decide(&self, gpu: &str, budget_mb: u64, key: &str, need_mb: u64, exclusive: Option<&str>) -> Step {
        let mut gpus = self.gpus.lock().unwrap();
        let list = gpus.entry(gpu.to_string()).or_default();
        if let Some(o) = list.iter_mut().find(|o| o.key == key) {
            o.active += 1;
            o.last_used = Instant::now();
            return Step::Granted;
        }
        let conflicts = |o: &Occupant| exclusive.is_some_and(|p| o.key.starts_with(p));
        let used: u64 = list.iter().map(|o| o.vram_mb).sum();
        let must_go: Vec<usize> = (0..list.len()).filter(|&i| conflicts(&list[i])).collect();
        let freed_by_must: u64 = must_go.iter().map(|&i| list[i].vram_mb).sum();
        let fits = |freed: u64| used - freed + need_mb <= budget_mb;
        if must_go.is_empty() && (fits(0) || list.is_empty()) {
            list.push(Occupant { key: key.into(), vram_mb: need_mb, active: 1, last_used: Instant::now() });
            return Step::Granted;
        }
        // Choose who leaves: the conflicting ones, then idle ones, least recently used first,
        // until it fits (or everything idle is gone: then it gets the GPU alone or waits).
        let mut leaving: Vec<usize> = must_go.clone();
        let mut freed = freed_by_must;
        let mut idle: Vec<usize> = (0..list.len()).filter(|&i| !leaving.contains(&i) && list[i].active == 0).collect();
        idle.sort_by_key(|&i| list[i].last_used);
        for i in idle {
            if fits(freed) {
                break;
            }
            leaving.push(i);
            freed += list[i].vram_mb;
        }
        let blocked = leaving.iter().any(|&i| list[i].active > 0)
            || (!fits(freed) && list.iter().enumerate().any(|(i, o)| !leaving.contains(&i) && o.active > 0));
        if blocked {
            // Busy models in the way: let the idle ones stay until the busy ones finish.
            return Step::Wait;
        }
        if leaving.is_empty() {
            // Nothing else is here; too big for the budget, but it can have the GPU alone.
            list.push(Occupant { key: key.into(), vram_mb: need_mb, active: 1, last_used: Instant::now() });
            return Step::Granted;
        }
        leaving.sort_unstable_by(|a, b| b.cmp(a));
        let keys = leaving.into_iter().map(|i| list.remove(i).key).collect();
        Step::Evict(keys)
    }

    /// Forget a model that stopped (crashed, or was stopped from outside the arbiter).
    pub fn remove(&self, key: &str) {
        let mut gpus = self.gpus.lock().unwrap();
        for list in gpus.values_mut() {
            list.retain(|o| o.key != key);
        }
        drop(gpus);
        self.changed.notify_waiters();
    }

    /// Take idle models out (all idle ones, or those unused for `older_than`) and return their
    /// keys for stopping.
    pub fn take_idle(&self, older_than: Option<Duration>, only: Option<&str>) -> Vec<String> {
        let mut gpus = self.gpus.lock().unwrap();
        let mut out = vec![];
        for list in gpus.values_mut() {
            list.retain(|o| {
                let stale = older_than.is_none_or(|d| o.last_used.elapsed() >= d);
                let matches = only.is_none_or(|k| o.key == k);
                if o.active == 0 && stale && matches {
                    out.push(o.key.clone());
                    false
                } else {
                    true
                }
            });
        }
        out
    }

    /// (gpu, occupant) for every loaded model.
    pub fn snapshot(&self) -> Vec<(String, Occupant)> {
        let gpus = self.gpus.lock().unwrap();
        let mut out: Vec<(String, Occupant)> =
            gpus.iter().flat_map(|(g, list)| list.iter().map(move |o| (g.clone(), o.clone()))).collect();
        out.sort_by(|a, b| (&a.0, &a.1.key).cmp(&(&b.0, &b.1.key)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stopped() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(vec![]))
    }

    #[tokio::test]
    async fn shares_when_it_fits_and_swaps_when_not() {
        let a = Arbiter::new();
        let log = stopped();
        let stop = |k: String| {
            let log = log.clone();
            async move { log.lock().unwrap().push(k) }
        };
        // A 32 GB card: Qwen (20 GB) then Z-Image (14 GB) don't fit together.
        let qwen = a.acquire("cuda:0", 30_000, "llm:qwen", 20_000, None, stop).await;
        drop(qwen);
        let small = a.acquire("cuda:0", 30_000, "llm:tiny", 2_000, None, stop).await;
        assert!(log.lock().unwrap().is_empty(), "fits beside qwen");
        drop(small);
        let img = a.acquire("cuda:0", 30_000, "sd:cuda:0:z", 14_000, Some("sd:cuda:0:"), stop).await;
        assert_eq!(*log.lock().unwrap(), vec!["llm:qwen"], "least recently used idle model leaves first");
        // The same model again is just another user of it.
        let img2 = a.acquire("cuda:0", 30_000, "sd:cuda:0:z", 14_000, Some("sd:cuda:0:"), stop).await;
        drop((img, img2));
        // The video model shares the server's slot: the image model goes even though both fit.
        let _vid = a.acquire("cuda:0", 60_000, "sd:cuda:0:wan", 20_000, Some("sd:cuda:0:"), stop).await;
        assert_eq!(log.lock().unwrap().last().unwrap(), "sd:cuda:0:z");
        let keys: Vec<String> = a.snapshot().into_iter().map(|(_, o)| o.key).collect();
        assert_eq!(keys, ["llm:tiny", "sd:cuda:0:wan"]);
        // Other GPUs are separate.
        let _chat = a.acquire("vulkan:0", 40_000, "llm:gemma", 22_000, None, stop).await;
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn waits_for_a_busy_model() {
        let a = Arbiter::new();
        let log = stopped();
        let busy = a.acquire("cuda:0", 30_000, "llm:qwen", 20_000, None, |_| async {}).await;
        let a2 = a.clone();
        let log2 = log.clone();
        let waiter = tokio::spawn(async move {
            let _l = a2
                .acquire("cuda:0", 30_000, "sd:cuda:0:z", 14_000, None, |k| {
                    let log = log2.clone();
                    async move { log.lock().unwrap().push(k) }
                })
                .await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiter.is_finished(), "qwen is busy");
        drop(busy);
        tokio::time::timeout(Duration::from_secs(2), waiter).await.unwrap().unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["llm:qwen"]);
    }

    #[tokio::test]
    async fn too_big_alone_still_runs_and_idle_reaping() {
        let a = Arbiter::new();
        let l = a.acquire("vulkan:0", 10_000, "llm:huge", 50_000, None, |_| async {}).await;
        drop(l);
        assert!(a.take_idle(Some(Duration::from_secs(3600)), None).is_empty(), "not stale yet");
        assert_eq!(a.take_idle(None, Some("llm:other")), Vec::<String>::new());
        assert_eq!(a.take_idle(None, None), vec!["llm:huge"]);
        assert!(a.snapshot().is_empty());
    }
}
