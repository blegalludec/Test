//! Exécution parallèle des actions sur plusieurs postes, hors du thread GTK.

use crate::actions::Action;
use crate::inventory::{Host, Settings};
use crate::ssh::{self, CommandOutput};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone)]
pub struct Job {
    pub host: Host,
    pub action: Action,
}

#[derive(Debug, Clone)]
pub enum JobEvent {
    Started {
        host: String,
        action: Action,
        command: String,
    },
    Finished {
        host: String,
        action: Action,
        result: Result<CommandOutput, String>,
    },
}

/// Lance les tâches avec au plus `settings.max_parallel` connexions
/// simultanées. Les événements sont envoyés sur `tx` ; la fonction rend la
/// main immédiatement.
pub fn spawn(jobs: Vec<Job>, settings: Settings, tx: async_channel::Sender<JobEvent>) {
    if jobs.is_empty() {
        return;
    }
    let workers = settings.max_parallel.clamp(1, 64).min(jobs.len());
    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
    let settings = Arc::new(settings);
    for _ in 0..workers {
        let queue = Arc::clone(&queue);
        let settings = Arc::clone(&settings);
        let tx = tx.clone();
        thread::spawn(move || {
            while let Some(job) = queue.lock().ok().and_then(|mut q| q.pop_front()) {
                let host = job.host.name.clone();
                let result = match job.action.remote_command(&job.host.user) {
                    Err(e) => Err(e),
                    Ok(cmd) => {
                        let _ = tx.send_blocking(JobEvent::Started {
                            host: host.clone(),
                            action: job.action.clone(),
                            command: cmd.clone(),
                        });
                        ssh::run(&job.host, &settings, &cmd)
                            .map_err(|e| format!("impossible de lancer ssh : {e}"))
                    }
                };
                if tx
                    .send_blocking(JobEvent::Finished {
                        host,
                        action: job.action,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
    }
}
